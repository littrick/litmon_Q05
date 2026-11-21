//! Q0.5 GPUI 界面与视频渲染。
//!
//! 当前最小实现：主窗口显示 Q0.5 视频流，并在每帧释放旧 `RenderImage`
//! 的 sprite atlas 条目（见 [`VideoView::render`]）。
//!
//! LF0 实测：每帧必须调用 `cx.drop_image(old, Some(window))`；
//! 仅 drop `Arc<RenderImage>` 会导致 atlas 内存持续增长。

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use async_channel::Receiver;
use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, ImageSource, IntoElement, Render, RenderImage, Window, WindowBounds,
    WindowOptions, div, img, px, size,
};
use gpui_platform::application;
use image::{Frame as ImageFrame, ImageBuffer, Rgba};
use q05_video::{Frame, VideoError, VideoEvent};

/// 视频视图。
struct VideoView {
    latest: Option<Arc<RenderImage>>,
    retired: Vec<Arc<RenderImage>>,
    status: String,
    frames: u64,
    last_report: Instant,
    last_frames: u64,
    latency_sum: Duration,
    latency_count: u64,
    latency_max: Duration,
    width: u32,
    height: u32,
}

impl VideoView {
    fn new(rx: Receiver<VideoEvent>, cx: &mut Context<Self>) -> Self {
        cx.spawn(async move |this, cx| {
            while let Ok(event) = rx.recv().await {
                let alive = this
                    .update(cx, |view, cx| {
                        view.handle_event(event);
                        cx.notify();
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();

        Self {
            latest: None,
            retired: Vec::new(),
            status: "正在启动 Q0.5 视频…".to_string(),
            frames: 0,
            last_report: Instant::now(),
            last_frames: 0,
            latency_sum: Duration::ZERO,
            latency_count: 0,
            latency_max: Duration::ZERO,
            width: 0,
            height: 0,
        }
    }

    fn handle_event(&mut self, event: VideoEvent) {
        match event {
            VideoEvent::Frame(frame) => self.set_frame(frame),
            VideoEvent::Error(err) => {
                tracing::error!(error = %err, "video capture error");
                self.status = format!("视频错误：{err}");
            }
            VideoEvent::Stopped => {
                tracing::info!("video capture stopped");
                self.status = "视频已停止".to_string();
            }
        }
    }

    fn set_frame(&mut self, frame: Frame) {
        let now = Instant::now();
        let latency = now.saturating_duration_since(frame.captured_at);
        self.latency_sum += latency;
        self.latency_count += 1;
        self.latency_max = self.latency_max.max(latency);

        let buffer =
            ImageBuffer::<Rgba<u8>, Vec<u8>>::from_raw(frame.width, frame.height, frame.bgra)
                .expect("BGRA buffer size");
        let image = Arc::new(RenderImage::new([ImageFrame::new(buffer)]));
        if let Some(old) = self.latest.replace(image) {
            self.retired.push(old);
            // 保留一帧，等下一次 render 真正移除 atlas 条目。
            if self.retired.len() > 2 {
                self.retired.remove(0);
            }
        }
        self.frames += 1;
        self.width = frame.width;
        self.height = frame.height;
        self.status = format!(
            "{}x{}  ·  捕获→UI {:.1} ms",
            frame.width,
            frame.height,
            latency.as_secs_f64() * 1000.0
        );
    }

    fn report(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.last_report) < Duration::from_secs(1) {
            return;
        }
        let dt = now.duration_since(self.last_report).as_secs_f64();
        let fps = (self.frames - self.last_frames) as f64 / dt;
        let avg_latency = if self.latency_count > 0 {
            self.latency_sum.as_secs_f64() * 1000.0 / self.latency_count as f64
        } else {
            0.0
        };
        tracing::info!(
            fps,
            frames = self.frames,
            avg_latency_ms = avg_latency,
            max_latency_ms = self.latency_max.as_secs_f64() * 1000.0,
            width = self.width,
            height = self.height,
            "ui video stats"
        );
        self.last_report = now;
        self.last_frames = self.frames;
        self.latency_sum = Duration::ZERO;
        self.latency_count = 0;
        self.latency_max = Duration::ZERO;
    }
}

impl Render for VideoView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 关键：把上一帧从 sprite atlas 移除；只 drop Arc 不够。
        for old in self.retired.drain(..) {
            cx.drop_image(old, Some(window));
        }
        self.report();

        let content = if let Some(image) = &self.latest {
            img(ImageSource::Render(image.clone()))
                .size_full()
                .into_any_element()
        } else {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(gpui::white())
                .child(self.status.clone())
                .into_any_element()
        };

        div().size_full().bg(gpui::black()).child(content)
    }
}

/// 打开主窗口并开始消费视频事件。
///
/// 该函数会阻塞直到应用退出。
pub fn run(rx: Receiver<VideoEvent>) -> Result<()> {
    let open_error = Arc::new(std::sync::Mutex::new(None::<String>));
    let open_error_in = open_error.clone();
    application().run(move |cx: &mut App| {
        let bounds = Bounds::new(gpui::point(px(50.0), px(50.0)), size(px(1280.0), px(720.0)));
        match cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            move |_window, cx| cx.new(|cx| VideoView::new(rx, cx)),
        ) {
            Ok(_) => {
                cx.activate(true);
            }
            Err(err) => {
                *open_error_in.lock().expect("open_error mutex") =
                    Some(format!("open window failed: {err:?}"));
            }
        }
    });
    match open_error.lock().expect("open_error mutex").take() {
        Some(message) => Err(anyhow!(message)),
        None => Ok(()),
    }
}

/// 方便应用层构造错误。
pub fn video_error(err: VideoError) -> anyhow::Error {
    anyhow!("video: {err}")
}
