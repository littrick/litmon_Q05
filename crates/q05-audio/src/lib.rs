//! Q0.5 UAC 音频采集与本地播放（WASAPI / cpal）。
//!
//! LF0 实测（本机）：
//! - 输入：`数字音频接口 (Oray Q0.5)`，F32 / 2ch / 48000；
//! - 默认输出：`扬声器 (Realtek(R) Audio)`，F32 / 2ch / 48000。
//!
//! 当前实现假设输入/输出都是 F32；若不是，会在支持的配置中选一个 F32、
//! 2 声道、48kHz 的配置。暂不做重采样。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::HeapRb;
use ringbuf::traits::{Consumer, Producer, Split};

/// 采样率。
pub const SAMPLE_RATE: u32 = 48_000;
/// 声道数。
pub const CHANNELS: u16 = 2;
/// 环形缓冲时长（秒）。
const BUFFER_SECONDS: usize = 1;

/// 音频错误。
#[derive(Debug, Clone, thiserror::Error)]
pub enum AudioError {
    /// 未找到 Q0.5 音频采集端点。
    #[error("Oray Q0.5 audio input not found")]
    InputNotFound,
    /// 没有默认输出设备。
    #[error("no default output device")]
    OutputNotFound,
    /// 设备不支持 F32 格式。
    #[error("no supported F32 stream config for {0}")]
    UnsupportedFormat(String),
    /// cpal 错误。
    #[error("cpal: {0}")]
    Cpal(String),
}

fn cpal_err(err: cpal::Error) -> AudioError {
    AudioError::Cpal(format!("{err}"))
}

#[derive(Debug, Default)]
struct Stats {
    captured: AtomicU64,
    played: AtomicU64,
    underruns: AtomicU64,
    overruns: AtomicU64,
}

/// 音频统计快照（单位：样本数）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AudioStatsSnapshot {
    /// 从 Q0.5 采集到的样本数。
    pub captured_samples: u64,
    /// 播放到默认输出的样本数。
    pub played_samples: u64,
    /// 输出欠载次数。
    pub underruns: u64,
    /// 输入溢出次数。
    pub overruns: u64,
}

/// 实际使用的音频配置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioConfigInfo {
    /// 输入设备名。
    pub input_device: String,
    /// 输入采样率。
    pub input_rate: u32,
    /// 输入声道数。
    pub input_channels: u16,
    /// 输出设备名。
    pub output_device: String,
    /// 输出采样率。
    pub output_rate: u32,
    /// 输出声道数。
    pub output_channels: u16,
}

/// 本地静音开关（可在 UI/音频线程间共享）。
///
/// 静音只影响本地播放，采集与 HID/视频链路不受影响；
/// 关闭静音后不会补播静音期间的旧音频。
#[derive(Clone, Debug, Default)]
pub struct AudioMute(Arc<AtomicBool>);

impl AudioMute {
    /// 新建开关，默认不静音。
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前是否静音。
    pub fn is_muted(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// 设置静音状态。
    pub fn set_muted(&self, muted: bool) {
        self.0.store(muted, Ordering::Relaxed);
    }

    /// 切换静音状态，返回切换后的值。
    pub fn toggle(&self) -> bool {
        let muted = !self.is_muted();
        self.set_muted(muted);
        muted
    }
}

/// 音频播放引擎。
///
/// 创建后立即开始采集与播放；drop 时停止。
pub struct AudioEngine {
    _input: cpal::Stream,
    _output: cpal::Stream,
    stats: Arc<Stats>,
    config: AudioConfigInfo,
}

impl AudioEngine {
    /// 打开 Q0.5 输入与默认输出，开始播放。
    ///
    /// `mute` 控制本地输出；静音时仍然消费采集数据，避免恢复后补播旧音频。
    pub fn start(mute: AudioMute) -> Result<Self, AudioError> {
        let host = cpal::default_host();

        let input_device = find_q05_input(&host)?;
        let output_device = host
            .default_output_device()
            .ok_or(AudioError::OutputNotFound)?;

        let input_name = device_name(&input_device);
        let output_name = device_name(&output_device);
        let input_config = choose_input_config(&input_device)?;
        let output_config = choose_output_config(&output_device)?;

        if input_config.sample_rate() != output_config.sample_rate()
            || input_config.channels() != output_config.channels()
        {
            tracing::warn!(
                input_rate = input_config.sample_rate(),
                output_rate = output_config.sample_rate(),
                input_channels = input_config.channels(),
                output_channels = output_config.channels(),
                "audio input/output format mismatch; no resampling implemented"
            );
        }

        let stats = Arc::new(Stats::default());
        let buffer_samples = SAMPLE_RATE as usize * CHANNELS as usize * BUFFER_SECONDS;
        let (mut producer, mut consumer) = HeapRb::<f32>::new(buffer_samples).split();

        let capture_stats = stats.clone();
        let input_stream = input_device
            .build_input_stream::<f32, _, _>(
                input_config.config(),
                move |data, _info| {
                    capture_stats
                        .captured
                        .fetch_add(data.len() as u64, Ordering::Relaxed);
                    let written = producer.push_slice(data);
                    if written < data.len() {
                        capture_stats.overruns.fetch_add(1, Ordering::Relaxed);
                    }
                },
                |err| tracing::error!(error = %err, "audio input stream error"),
                None,
            )
            .map_err(cpal_err)?;

        let output_stats = stats.clone();
        let output_mute = mute.clone();
        let output_stream = output_device
            .build_output_stream::<f32, _, _>(
                output_config.config(),
                move |data, _info| {
                    // 静音时依旧消费采集数据，只把输出填成静音；
                    // 这样恢复后不会播放积压的旧音频。
                    let read = consumer.pop_slice(data);
                    if output_mute.is_muted() {
                        data.fill(0.0);
                    }
                    output_stats
                        .played
                        .fetch_add(read as u64, Ordering::Relaxed);
                    if read < data.len() {
                        output_stats.underruns.fetch_add(1, Ordering::Relaxed);
                    }
                },
                |err| tracing::error!(error = %err, "audio output stream error"),
                None,
            )
            .map_err(cpal_err)?;

        input_stream.play().map_err(cpal_err)?;
        output_stream.play().map_err(cpal_err)?;

        let config = AudioConfigInfo {
            input_device: input_name,
            input_rate: input_config.sample_rate(),
            input_channels: input_config.channels(),
            output_device: output_name,
            output_rate: output_config.sample_rate(),
            output_channels: output_config.channels(),
        };
        tracing::info!(
            input = %config.input_device,
            output = %config.output_device,
            rate = config.input_rate,
            channels = config.input_channels,
            "audio engine started"
        );

        Ok(Self {
            _input: input_stream,
            _output: output_stream,
            stats,
            config,
        })
    }

    /// 当前统计。
    pub fn stats(&self) -> AudioStatsSnapshot {
        AudioStatsSnapshot {
            captured_samples: self.stats.captured.load(Ordering::Relaxed),
            played_samples: self.stats.played.load(Ordering::Relaxed),
            underruns: self.stats.underruns.load(Ordering::Relaxed),
            overruns: self.stats.overruns.load(Ordering::Relaxed),
        }
    }

    /// 实际配置。
    pub fn config(&self) -> &AudioConfigInfo {
        &self.config
    }
}

fn device_name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|description| description.name().to_string())
        .unwrap_or_else(|_| "<unknown>".to_string())
}

fn is_q05_name(name: &str) -> bool {
    name.contains("Oray Q0.5") || name.contains("Q0.5")
}

fn find_q05_input(host: &cpal::Host) -> Result<cpal::Device, AudioError> {
    for device in host.input_devices().map_err(cpal_err)? {
        let name = device_name(&device);
        tracing::debug!(device = %name, "audio input candidate");
        if is_q05_name(&name) {
            return Ok(device);
        }
    }
    Err(AudioError::InputNotFound)
}

fn choose_input_config(device: &cpal::Device) -> Result<cpal::SupportedStreamConfig, AudioError> {
    let default = device.default_input_config().map_err(cpal_err)?;
    if default.sample_format() == cpal::SampleFormat::F32 {
        return Ok(default);
    }
    let name = device_name(device);
    for config in device.supported_input_configs().map_err(cpal_err)? {
        if config.sample_format() == cpal::SampleFormat::F32
            && config.channels() >= CHANNELS
            && config.min_sample_rate() <= SAMPLE_RATE
            && config.max_sample_rate() >= SAMPLE_RATE
        {
            return Ok(config.with_sample_rate(SAMPLE_RATE));
        }
    }
    Err(AudioError::UnsupportedFormat(name))
}

fn choose_output_config(device: &cpal::Device) -> Result<cpal::SupportedStreamConfig, AudioError> {
    let default = device.default_output_config().map_err(cpal_err)?;
    if default.sample_format() == cpal::SampleFormat::F32 {
        return Ok(default);
    }
    let name = device_name(device);
    for config in device.supported_output_configs().map_err(cpal_err)? {
        if config.sample_format() == cpal::SampleFormat::F32
            && config.channels() >= CHANNELS
            && config.min_sample_rate() <= SAMPLE_RATE
            && config.max_sample_rate() >= SAMPLE_RATE
        {
            return Ok(config.with_sample_rate(SAMPLE_RATE));
        }
    }
    Err(AudioError::UnsupportedFormat(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_q05_device_names() {
        assert!(is_q05_name("数字音频接口 (Oray Q0.5)"));
        assert!(is_q05_name("Oray Q0.5"));
        assert!(!is_q05_name("扬声器 (Realtek(R) Audio)"));
    }

    #[test]
    fn stats_default_is_zero() {
        let stats = AudioStatsSnapshot::default();
        assert_eq!(stats.captured_samples, 0);
        assert_eq!(stats.underruns, 0);
    }

    #[test]
    fn mute_switch_toggles_and_is_shared() {
        let mute = AudioMute::new();
        assert!(!mute.is_muted());
        let other = mute.clone();
        assert!(other.toggle());
        assert!(mute.is_muted());
        other.set_muted(false);
        assert!(!mute.is_muted());
    }
}
