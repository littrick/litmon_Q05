//! 媒体流 supervisor：监听设备插拔并启动/停止/重建视频、HID、音频。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use async_channel::Sender;
use q05_audio::AudioEngine;
use q05_hid::HidHandle;
use q05_video::{VideoCapture, VideoEvent, VideoFormat};

use crate::presence::{DeviceMonitor, DevicePresence};

/// 管理视频/HID/音频三条流的后台 supervisor。
///
/// `video_tx` / `hid_tx` / `presence_tx` 由 UI 侧持有接收端；
/// supervisor 在设备在线时启动流，断开时停止，重新插入后重建。
pub struct MediaSupervisor {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl MediaSupervisor {
    /// 启动 supervisor。
    pub fn start(
        video_tx: Sender<VideoEvent>,
        hid_tx: Sender<Option<HidHandle>>,
        presence_tx: Sender<DevicePresence>,
    ) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let thread = thread::Builder::new()
            .name("q05-media".into())
            .spawn(move || run(video_tx, hid_tx, presence_tx, stop_thread))?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }

    /// 停止并等待 supervisor。
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for MediaSupervisor {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run(
    video_tx: Sender<VideoEvent>,
    hid_tx: Sender<Option<HidHandle>>,
    presence_tx: Sender<DevicePresence>,
    stop: Arc<AtomicBool>,
) {
    let (monitor_tx, monitor_rx) = async_channel::bounded(8);
    let monitor = match DeviceMonitor::start(monitor_tx) {
        Ok(monitor) => monitor,
        Err(err) => {
            tracing::error!(error = %err, "device monitor start failed");
            let _ = presence_tx.try_send(DevicePresence::default());
            return;
        }
    };

    let mut connected = false;
    let mut video: Option<VideoCapture> = None;
    let mut audio: Option<AudioEngine> = None;
    let mut hid_ready = false;
    let mut next_retry = Instant::now();

    while !stop.load(Ordering::Relaxed) {
        while let Ok(presence) = monitor_rx.try_recv() {
            connected = presence.connected();
            let _ = presence_tx.try_send(presence);
            tracing::info!(?presence, "media supervisor presence");
            if !connected {
                if let Some(mut capture) = video.take() {
                    capture.stop();
                    tracing::info!("video stopped (device disconnected)");
                }
                if hid_ready {
                    let _ = hid_tx.try_send(None);
                    hid_ready = false;
                    tracing::info!("input hidden (device disconnected)");
                }
                audio = None;
                tracing::info!("audio stopped (device disconnected)");
                next_retry = Instant::now();
            }
        }

        if let Some(capture) = &video
            && capture.is_finished()
        {
            tracing::warn!("video capture thread exited; rebuilding");
            if hid_ready {
                let _ = hid_tx.try_send(None);
                hid_ready = false;
            }
            video.take();
            next_retry = Instant::now() + Duration::from_secs(1);
        }

        if connected && Instant::now() >= next_retry {
            if video.is_none() {
                match VideoCapture::start(VideoFormat::FHD30, video_tx.clone()) {
                    Ok(capture) => {
                        video = Some(capture);
                        tracing::info!("video started");
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "video start failed; will retry");
                    }
                }
            }

            let video_ready = video
                .as_ref()
                .is_some_and(|capture| capture.stats().frames > 0);
            if video_ready && !hid_ready {
                match HidHandle::open() {
                    Ok(hid) => match hid_tx.try_send(Some(hid)) {
                        Ok(()) => {
                            hid_ready = true;
                            tracing::info!("input HID started");
                        }
                        Err(err) => {
                            tracing::warn!(error = %err, "send HID to UI failed; will retry");
                        }
                    },
                    Err(err) => {
                        tracing::warn!(error = %err, "input HID open failed; will retry");
                    }
                }
            }

            if audio.is_none() {
                match AudioEngine::start() {
                    Ok(engine) => {
                        audio = Some(engine);
                        tracing::info!("audio started");
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "audio start failed; will retry");
                    }
                }
            }

            next_retry = Instant::now() + Duration::from_secs(1);
        }

        thread::sleep(Duration::from_millis(200));
    }

    if let Some(mut capture) = video.take() {
        capture.stop();
    }
    if hid_ready {
        let _ = hid_tx.try_send(None);
    }
    drop(audio);
    drop(monitor);
}
