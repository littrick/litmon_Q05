//! Q0.5 视频采集（UVC / Media Foundation）。
//!
//! 本 crate 负责：
//! - 枚举并匹配 “Oray Q0.5” 摄像头；
//! - 通过 `IMFSourceReader` 读取 native MJPEG；
//! - 用 `zune-jpeg` 解码为 BGRA；
//! - 通过有界 channel 发布最新帧。
//!
//! LF0 真机验证结论：
//! - native MJPEG 1080p30 可用，解码约 10ms/帧；
//! - 产品固定使用**独占采集**（`share_mode=0`）。运行时若相机被 AweSun 占用，
//!   需要提示用户退出 AweSun 的 Q0.5 窗口/进程。
//! - 共享模式实验已完成，但**不纳入产品需求**。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use async_channel::Sender;

/// 请求的视频格式（实际帧尺寸以 [`Frame`] 为准）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoFormat {
    /// 宽度（像素）。
    pub width: u32,
    /// 高度（像素）。
    pub height: u32,
    /// 帧率。
    pub fps: u32,
}

impl VideoFormat {
    /// 1080p30。
    pub const FHD30: Self = Self {
        width: 1920,
        height: 1080,
        fps: 30,
    };

    /// 720p30。
    pub const HD30: Self = Self {
        width: 1280,
        height: 720,
        fps: 30,
    };
}

impl Default for VideoFormat {
    fn default() -> Self {
        Self::FHD30
    }
}

/// 一帧已解码的 BGRA 图像。
#[derive(Debug, Clone)]
pub struct Frame {
    /// 宽度。
    pub width: u32,
    /// 高度。
    pub height: u32,
    /// BGRA 像素，长度 = width * height * 4。
    pub bgra: Vec<u8>,
    /// 从 MF 收到 sample 的时间。
    pub captured_at: Instant,
    /// JPEG 解码 + BGRA 转换完成的时间。
    pub decoded_at: Instant,
}

impl Frame {
    /// 一帧的字节数。
    pub fn len_bytes(&self) -> usize {
        self.bgra.len()
    }

    /// 解码耗时。
    pub fn decode_duration(&self) -> Duration {
        self.decoded_at.saturating_duration_since(self.captured_at)
    }
}

/// 视频采集错误。
#[derive(Debug, Clone, thiserror::Error)]
pub enum VideoError {
    /// 未找到 “Oray Q0.5” 摄像头。
    #[error("Oray Q0.5 camera not found")]
    NotFound,
    /// 当前平台不支持（目前只实现 Windows）。
    #[error("video capture is not supported on this platform")]
    Unsupported,
    /// Windows / Media Foundation 错误。
    #[error("media foundation: {0}")]
    Windows(String),
    /// JPEG 数据无效或格式不支持。
    #[error("jpeg: {0}")]
    Jpeg(String),
    /// 解码器没有给出图像信息。
    #[error("jpeg decoder returned no image info")]
    NoInfo,
    /// 采集线程异常退出。
    #[error("video thread: {0}")]
    Thread(String),
}

mod screen;
pub use screen::ScreenState;

/// 采集线程发布的事件。
#[derive(Debug, Clone)]
pub enum VideoEvent {
    /// 新帧。
    Frame(Frame),
    /// 目标屏幕状态变化（正常/无信号/息屏）。
    ScreenState(ScreenState),
    /// 采集错误（线程即将退出）。
    Error(VideoError),
    /// 线程已停止。
    Stopped,
}

/// 采集统计快照。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VideoStatsSnapshot {
    /// 成功解码并尝试发布的帧数。
    pub frames: u64,
    /// 因 channel 满而丢弃的帧数。
    pub drops: u64,
    /// JPEG 解码失败次数。
    pub decode_errors: u64,
    /// 平均解码耗时（微秒）。
    pub avg_decode_us: u64,
}

#[derive(Debug, Default)]
struct Stats {
    frames: AtomicU64,
    drops: AtomicU64,
    decode_errors: AtomicU64,
    decode_total_us: AtomicU64,
    decode_count: AtomicU64,
}

impl Stats {
    fn snapshot(&self) -> VideoStatsSnapshot {
        let count = self.decode_count.load(Ordering::Relaxed);
        let total = self.decode_total_us.load(Ordering::Relaxed);
        VideoStatsSnapshot {
            frames: self.frames.load(Ordering::Relaxed),
            drops: self.drops.load(Ordering::Relaxed),
            decode_errors: self.decode_errors.load(Ordering::Relaxed),
            avg_decode_us: total.checked_div(count).unwrap_or(0),
        }
    }
}

/// 正在运行的视频采集。
///
/// 调用 [`VideoCapture::start`] 后，采集线程持续通过 `event_tx` 发布
/// [`VideoEvent::Frame`]。调用 [`VideoCapture::stop`] 或 drop 会停止并 join 线程。
pub struct VideoCapture {
    stop: Arc<AtomicBool>,
    stats: Arc<Stats>,
    thread: Option<JoinHandle<()>>,
}

impl VideoCapture {
    /// 启动采集线程。
    ///
    /// `event_tx` 建议使用容量 2 的有界 channel；UI 慢时新帧会被丢弃，
    /// 而不是无限堆积。
    pub fn start(format: VideoFormat, event_tx: Sender<VideoEvent>) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(Stats::default());
        let stop_thread = stop.clone();
        let stats_thread = stats.clone();
        let thread = thread::Builder::new()
            .name("q05-video".into())
            .spawn(move || {
                let result = platform::run(format, &event_tx, &stop_thread, &stats_thread);
                match result {
                    Ok(()) => {
                        let _ = event_tx.try_send(VideoEvent::Stopped);
                    }
                    Err(err) => {
                        tracing::error!(error = %err, "video capture stopped with error");
                        let _ = event_tx.try_send(VideoEvent::Error(err));
                    }
                }
            })?;
        Ok(Self {
            stop,
            stats,
            thread: Some(thread),
        })
    }

    /// 当前统计。
    pub fn stats(&self) -> VideoStatsSnapshot {
        self.stats.snapshot()
    }

    /// 采集线程是否已经退出（错误、设备拔出或 stop）。
    pub fn is_finished(&self) -> bool {
        self.thread
            .as_ref()
            .is_none_or(std::thread::JoinHandle::is_finished)
    }

    /// 请求停止并等待采集线程退出。
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for VideoCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 把 RGB8 转成 GPUI `RenderImage` 需要的 BGRA。
fn rgb_to_bgra(rgb: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    let pixels = (width as usize).checked_mul(height as usize)?;
    if rgb.len() != pixels.checked_mul(3)? {
        return None;
    }
    let mut bgra = vec![0u8; pixels.checked_mul(4)?];
    for (i, src) in rgb.chunks_exact(3).enumerate() {
        let o = i * 4;
        bgra[o] = src[2];
        bgra[o + 1] = src[1];
        bgra[o + 2] = src[0];
        bgra[o + 3] = 255;
    }
    Some(bgra)
}

#[cfg(windows)]
mod platform {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    use async_channel::Sender;
    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::{
        COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize,
    };
    use windows::core::GUID;

    use crate::{Frame, Stats, VideoError, VideoEvent, VideoFormat, rgb_to_bgra};

    // Windows SDK mfidl.h:
    // EXTERN_GUID(MF_DEVSOURCE_ATTRIBUTE_FRAMESERVER_SHARE_MODE,
    //   0x44d1a9bc, 0x2999, 0x4238, 0xae, 0x43, 0x07, 0x30, 0xce, 0xb2, 0xab, 0x1b);
    const FRAMESERVER_SHARE_MODE: GUID = GUID::from_u128(0x44d1a9bc_2999_4238_ae43_0730ceb2ab1b);

    struct ComGuard;

    impl Drop for ComGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = MFShutdown();
                CoUninitialize();
            }
        }
    }

    fn win_err(err: windows::core::Error) -> VideoError {
        VideoError::Windows(format!("{err:?}"))
    }

    unsafe fn attr_string(attrs: &IMFAttributes, key: &GUID) -> Option<String> {
        let len = unsafe { attrs.GetStringLength(key) }.ok()?;
        let mut buf = vec![0u16; len as usize + 1];
        let mut got = 0u32;
        unsafe { attrs.GetString(key, &mut buf, Some(&mut got)) }.ok()?;
        Some(String::from_utf16_lossy(&buf[..got as usize]))
    }

    unsafe fn enumerate_video_devices() -> Result<Vec<IMFActivate>, VideoError> {
        let mut attrs: Option<IMFAttributes> = None;
        unsafe { MFCreateAttributes(&mut attrs, 1) }.map_err(win_err)?;
        let attrs = attrs.ok_or_else(|| VideoError::Windows("MFCreateAttributes None".into()))?;
        unsafe {
            attrs.SetGUID(
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
                &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
            )
        }
        .map_err(win_err)?;

        let mut devices: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count: u32 = 0;
        unsafe { MFEnumDeviceSources(&attrs, &mut devices, &mut count) }.map_err(win_err)?;

        let mut out = Vec::new();
        if !devices.is_null() {
            for i in 0..count as usize {
                let slot = unsafe { &*devices.add(i) };
                if let Some(activate) = slot {
                    out.push(activate.clone());
                }
            }
            unsafe { CoTaskMemFree(Some(devices as *const core::ffi::c_void)) };
        }
        Ok(out)
    }

    /// 每次返回独立的 `IMFActivate`（同一个 activate 会缓存已激活的 source）。
    unsafe fn find_target() -> Result<IMFActivate, VideoError> {
        let devices = unsafe { enumerate_video_devices()? };
        for activate in &devices {
            let name = unsafe { attr_string(activate, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME) };
            if name.as_deref().is_some_and(|n| n.contains("Oray Q0.5")) {
                return Ok(activate.clone());
            }
        }
        Err(VideoError::NotFound)
    }

    unsafe fn create_reader(format: VideoFormat) -> Result<IMFSourceReader, VideoError> {
        let activate = unsafe { find_target()? };
        // 产品固定独占采集。
        unsafe { activate.SetUINT32(&FRAMESERVER_SHARE_MODE, 0) }.map_err(win_err)?;
        let source: IMFMediaSource = unsafe { activate.ActivateObject() }.map_err(win_err)?;

        let mut attrs: Option<IMFAttributes> = None;
        unsafe { MFCreateAttributes(&mut attrs, 4) }.map_err(win_err)?;
        let attrs = attrs.ok_or_else(|| VideoError::Windows("MFCreateAttributes None".into()))?;
        unsafe {
            attrs
                .SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)
                .map_err(win_err)?;
            attrs.SetUINT32(&MF_LOW_LATENCY, 1).map_err(win_err)?;
            attrs
                .SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)
                .map_err(win_err)?;
            attrs
                .SetUINT32(&MF_SOURCE_READER_DISCONNECT_MEDIASOURCE_ON_SHUTDOWN, 1)
                .map_err(win_err)?;
        }
        let reader =
            unsafe { MFCreateSourceReaderFromMediaSource(&source, &attrs) }.map_err(win_err)?;

        let mt = unsafe { MFCreateMediaType() }.map_err(win_err)?;
        unsafe {
            mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .map_err(win_err)?;
            mt.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_MJPG)
                .map_err(win_err)?;
            mt.SetUINT64(
                &MF_MT_FRAME_SIZE,
                ((format.width as u64) << 32) | format.height as u64,
            )
            .map_err(win_err)?;
            mt.SetUINT64(&MF_MT_FRAME_RATE, ((format.fps as u64) << 32) | 1u64)
                .map_err(win_err)?;
            mt.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
                .map_err(win_err)?;
            reader
                .SetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, None, &mt)
                .map_err(win_err)?;
        }
        tracing::info!(
            width = format.width,
            height = format.height,
            fps = format.fps,
            "video: requested MJPEG format"
        );
        Ok(reader)
    }

    pub(crate) fn run(
        format: VideoFormat,
        event_tx: &Sender<VideoEvent>,
        stop: &Arc<AtomicBool>,
        stats: &Arc<Stats>,
    ) -> Result<(), VideoError> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, 0).map_err(win_err)?;
        }
        let _guard = ComGuard;

        let reader = unsafe { create_reader(format)? };
        let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
        let mut screen = crate::screen::ScreenDetector::new();

        while !stop.load(Ordering::Relaxed) {
            let mut flags = 0u32;
            let mut ts = 0i64;
            let mut sample: Option<IMFSample> = None;
            unsafe {
                reader.ReadSample(
                    stream,
                    0,
                    None,
                    Some(&mut flags),
                    Some(&mut ts),
                    Some(&mut sample),
                )
            }
            .map_err(win_err)?;

            if flags & MF_SOURCE_READERF_STREAMTICK.0 as u32 != 0 {
                continue;
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                break;
            }
            let Some(sample) = sample else { continue };
            let captured_at = Instant::now();

            let jpeg = unsafe {
                let buffer = sample.ConvertToContiguousBuffer().map_err(win_err)?;
                let mut ptr = std::ptr::null_mut();
                let mut len = 0u32;
                buffer
                    .Lock(&mut ptr, None, Some(&mut len))
                    .map_err(win_err)?;
                let copy = std::slice::from_raw_parts(ptr, len as usize).to_vec();
                buffer.Unlock().map_err(win_err)?;
                copy
            };

            let mut decoder = zune_jpeg::JpegDecoder::new(std::io::Cursor::new(&jpeg[..]));
            let pixels = match decoder.decode() {
                Ok(pixels) => pixels,
                Err(err) => {
                    stats.decode_errors.fetch_add(1, Ordering::Relaxed);
                    tracing::debug!(error = ?err, "jpeg decode error");
                    continue;
                }
            };
            let info = decoder.info().ok_or(VideoError::NoInfo)?;
            let (width, height) = (info.width as u32, info.height as u32);
            let Some(bgra) = rgb_to_bgra(&pixels, width, height) else {
                stats.decode_errors.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(
                    width,
                    height,
                    len = pixels.len(),
                    "unexpected jpeg output size"
                );
                continue;
            };
            let decoded_at = Instant::now();

            if let Some(state) = screen.analyze(&bgra, width, height) {
                tracing::info!(screen_state = ?state, "screen state changed");
                let _ = event_tx.try_send(VideoEvent::ScreenState(state));
            }

            stats.frames.fetch_add(1, Ordering::Relaxed);
            stats.decode_total_us.fetch_add(
                decoded_at
                    .saturating_duration_since(captured_at)
                    .as_micros() as u64,
                Ordering::Relaxed,
            );
            stats.decode_count.fetch_add(1, Ordering::Relaxed);

            let frame = Frame {
                width,
                height,
                bgra,
                captured_at,
                decoded_at,
            };
            if event_tx.try_send(VideoEvent::Frame(frame)).is_err() {
                stats.drops.fetch_add(1, Ordering::Relaxed);
            }
        }

        Ok(())
    }
}

#[cfg(not(windows))]
mod platform {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    use async_channel::Sender;

    use crate::{Stats, VideoError, VideoEvent, VideoFormat};

    pub(crate) fn run(
        _format: VideoFormat,
        _event_tx: &Sender<VideoEvent>,
        _stop: &Arc<AtomicBool>,
        _stats: &Arc<Stats>,
    ) -> Result<(), VideoError> {
        Err(VideoError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_to_bgra_swaps_red_blue() {
        let rgb = [255, 0, 0, 0, 255, 0, 0, 0, 255, 10, 20, 30];
        let bgra = rgb_to_bgra(&rgb, 2, 2).expect("valid size");
        assert_eq!(
            bgra,
            vec![
                0, 0, 255, 255, // red -> B=0 G=0 R=255
                0, 255, 0, 255, // green
                255, 0, 0, 255, // blue
                30, 20, 10, 255 // arbitrary
            ]
        );
    }

    #[test]
    fn rgb_to_bgra_rejects_wrong_length() {
        assert!(rgb_to_bgra(&[1, 2, 3], 2, 2).is_none());
    }

    #[test]
    fn frame_decode_duration_is_saturating() {
        let now = Instant::now();
        let frame = Frame {
            width: 1,
            height: 1,
            bgra: vec![0; 4],
            captured_at: now,
            decoded_at: now,
        };
        assert_eq!(frame.decode_duration(), Duration::ZERO);
        assert_eq!(frame.len_bytes(), 4);
    }

    #[test]
    fn default_format_is_1080p30() {
        assert_eq!(VideoFormat::default(), VideoFormat::FHD30);
    }
}
