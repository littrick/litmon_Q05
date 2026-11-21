//! Q0.5 GPUI 界面、视频渲染与窗口内输入透传。
//!
//! - 视频：MF 帧 → `RenderImage`；每帧必须 `cx.drop_image(old, Some(window))`。
//! - 输入：窗口内键鼠/滚轮 → HID usage/归一化坐标 → `413D:2107`。
//! - 安全：Ctrl+Alt+Q 开关；失焦/退出 release-all；不做全局钩子。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use async_channel::Receiver;
use gpui::prelude::*;
use gpui::{
    App, Bounds, Context, FocusHandle, ImageSource, IntoElement, KeyDownEvent, KeyUpEvent,
    MouseButton as GpuiMouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ObjectFit,
    Render, RenderImage, ScrollDelta, ScrollEvent, Subscription, Window, WindowBounds,
    WindowOptions, div, img, px, size,
};
use gpui_platform::application;
use image::{Frame as ImageFrame, ImageBuffer, Rgba};
use q05_hid::HidHandle;
use q05_input::{
    KeyboardState, Modifiers, MouseButton, MouseState, hid_usage_for_key,
    normalize_position_in_rect,
};
use q05_session::DevicePresence;
use q05_video::{Frame, ScreenState, VideoError, VideoEvent};

const MOUSE_SEND_INTERVAL: Duration = Duration::from_millis(8);

/// 视频 + 输入视图。
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
    focus_handle: FocusHandle,
    subscriptions: Vec<Subscription>,
    hid: Option<HidHandle>,
    keyboard: KeyboardState,
    mouse: MouseState,
    input_enabled: bool,
    last_mouse_send: Instant,
    mouse_logged: bool,
    log_all_mouse: bool,
    screen_state: ScreenState,
    device_present: bool,
    presence_seen: bool,
    device_message: Option<String>,
}

impl VideoView {
    fn new(
        rx: Receiver<VideoEvent>,
        presence_rx: Receiver<DevicePresence>,
        hid: Option<HidHandle>,
        cx: &mut Context<Self>,
    ) -> Self {
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

        cx.spawn(async move |this, cx| {
            while let Ok(presence) = presence_rx.recv().await {
                let alive = this
                    .update(cx, |view, cx| {
                        view.handle_presence(presence);
                        cx.notify();
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();

        let input_enabled = hid.is_some();
        Self {
            latest: None,
            retired: Vec::new(),
            status: if input_enabled {
                "正在启动 Q0.5 视频…".to_string()
            } else {
                "视频启动中；未找到输入 HID".to_string()
            },
            frames: 0,
            last_report: Instant::now(),
            last_frames: 0,
            latency_sum: Duration::ZERO,
            latency_count: 0,
            latency_max: Duration::ZERO,
            width: 0,
            height: 0,
            focus_handle: cx.focus_handle(),
            subscriptions: Vec::new(),
            hid,
            keyboard: KeyboardState::new(),
            mouse: MouseState::new(),
            input_enabled,
            last_mouse_send: Instant::now() - MOUSE_SEND_INTERVAL,
            mouse_logged: false,
            log_all_mouse: std::env::var_os("Q05_MOUSE_LOG").is_some(),
            screen_state: ScreenState::Live,
            device_present: false,
            presence_seen: false,
            device_message: None,
        }
    }

    fn handle_event(&mut self, event: VideoEvent) {
        match event {
            VideoEvent::Frame(frame) => self.set_frame(frame),
            VideoEvent::Error(err) => {
                tracing::error!(error = %err, "video capture error");
                // 独占模式：没有画面就不注入输入，避免与 AweSun 等抢占。
                self.release_all();
                self.input_enabled = false;
                self.hid = None;
                self.status =
                    format!("视频错误：{err}\n请退出 AweSun 的 Q0.5 窗口/进程后重启；输入已停用");
            }
            VideoEvent::ScreenState(state) => {
                self.screen_state = state;
                tracing::info!(screen_state = ?state, "target screen state");
            }
            VideoEvent::Stopped => {
                tracing::info!("video capture stopped");
                self.release_all();
                self.input_enabled = false;
                self.hid = None;
                self.status = "视频已停止；输入已停用".to_string();
            }
        }
    }

    fn handle_presence(&mut self, presence: DevicePresence) {
        let connected = presence.connected();
        if !self.presence_seen {
            self.presence_seen = true;
            self.device_present = connected;
            if !connected {
                self.device_message = Some("Q0.5 设备未连接".to_string());
                self.input_enabled = false;
                self.hid = None;
            }
            tracing::info!(?presence, "device presence initial");
            return;
        }

        let was_connected = self.device_present;
        self.device_present = connected;
        if was_connected && !connected {
            self.release_all();
            self.input_enabled = false;
            self.hid = None;
            self.device_message = Some("Q0.5 设备已断开".to_string());
            tracing::warn!(?presence, "device disconnected");
        } else if !was_connected && connected {
            self.device_message = Some("Q0.5 已重新连接，请重启应用以恢复视频".to_string());
            tracing::info!(?presence, "device reconnected");
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
        let hid = self.hid.as_ref().map(HidHandle::stats).unwrap_or_default();
        tracing::info!(
            fps,
            frames = self.frames,
            avg_latency_ms = avg_latency,
            max_latency_ms = self.latency_max.as_secs_f64() * 1000.0,
            width = self.width,
            height = self.height,
            input_enabled = self.input_enabled,
            screen_state = ?self.screen_state,
            hid_sent = hid.sent,
            hid_acks = hid.acks,
            hid_heartbeats = hid.heartbeats,
            "ui stats"
        );
        self.last_report = now;
        self.last_frames = self.frames;
        self.latency_sum = Duration::ZERO;
        self.latency_count = 0;
        self.latency_max = Duration::ZERO;
    }

    fn send_frame(&self, frame: q05_hid::InputFrame) {
        let Some(hid) = &self.hid else { return };
        if let Err(err) = hid.send(frame) {
            tracing::debug!(error = %err, "input send skipped");
        }
    }

    fn release_all(&mut self) {
        if let Some(frame) = self.keyboard.release_all() {
            self.send_frame(frame);
        }
        let frame = self.mouse.release_buttons();
        self.send_frame(frame);
    }

    fn update_mouse_position(&mut self, x: f32, y: f32, window: &Window) {
        let size = window.viewport_size();
        let viewport_w = f32::from(size.width);
        let viewport_h = f32::from(size.height);
        let (video_w, video_h) = if self.width > 0 && self.height > 0 {
            (self.width as f32, self.height as f32)
        } else {
            (1920.0, 1080.0)
        };
        let rect = content_rect(viewport_w, viewport_h, video_w, video_h);
        let (nx, ny) = normalize_position_in_rect(x, y, rect.x, rect.y, rect.width, rect.height);
        self.mouse.set_position(nx, ny);
        if !self.mouse_logged || self.log_all_mouse {
            self.mouse_logged = true;
            tracing::info!(
                local_x = x,
                local_y = y,
                viewport_w,
                viewport_h,
                video_w,
                video_h,
                rect_x = rect.x,
                rect_y = rect.y,
                rect_w = rect.width,
                rect_h = rect.height,
                norm_x = nx,
                norm_y = ny,
                "mouse mapping"
            );
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let modifiers = to_modifiers(&event.keystroke.modifiers);
        // 安全开关：Ctrl+Alt+Q。
        if modifiers.control && modifiers.alt && event.keystroke.key.eq_ignore_ascii_case("q") {
            self.input_enabled = !self.input_enabled && self.hid.is_some();
            if self.input_enabled {
                window.focus(&self.focus_handle, cx);
                tracing::info!("input enabled");
            } else {
                self.release_all();
                tracing::info!("input disabled");
            }
            return;
        }
        if !self.input_enabled || event.is_held {
            return;
        }
        if let Some(usage) = hid_usage_for_key(&event.keystroke.key) {
            if let Some(frame) = self.keyboard.key_down(usage, modifiers) {
                self.send_frame(frame);
            }
        } else if let Some(frame) = self.keyboard.modifiers_changed(modifiers) {
            self.send_frame(frame);
        }
    }

    fn on_key_up(&mut self, event: &KeyUpEvent) {
        if !self.input_enabled {
            return;
        }
        let modifiers = to_modifiers(&event.keystroke.modifiers);
        if let Some(usage) = hid_usage_for_key(&event.keystroke.key) {
            if let Some(frame) = self.keyboard.key_up(usage, modifiers) {
                self.send_frame(frame);
            }
        } else if let Some(frame) = self.keyboard.modifiers_changed(modifiers) {
            self.send_frame(frame);
        }
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, window: &Window) {
        if !self.input_enabled {
            return;
        }
        self.update_mouse_position(
            f32::from(event.position.x),
            f32::from(event.position.y),
            window,
        );
        let now = Instant::now();
        if now.duration_since(self.last_mouse_send) >= MOUSE_SEND_INTERVAL {
            self.last_mouse_send = now;
            let frame = self.mouse.frame(0);
            self.send_frame(frame);
        }
    }

    fn on_mouse_button(
        &mut self,
        button: GpuiMouseButton,
        pressed: bool,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.input_enabled {
            return;
        }
        window.focus(&self.focus_handle, cx);
        self.update_mouse_position(
            f32::from(event.position.x),
            f32::from(event.position.y),
            window,
        );
        let Some(button) = to_mouse_button(button) else {
            return;
        };
        if self.mouse.set_button(button, pressed) {
            let frame = self.mouse.frame(0);
            self.send_frame(frame);
        }
    }

    fn on_mouse_up_button(
        &mut self,
        button: GpuiMouseButton,
        event: &MouseUpEvent,
        window: &Window,
    ) {
        if !self.input_enabled {
            return;
        }
        self.update_mouse_position(
            f32::from(event.position.x),
            f32::from(event.position.y),
            window,
        );
        let Some(button) = to_mouse_button(button) else {
            return;
        };
        if self.mouse.set_button(button, false) {
            let frame = self.mouse.frame(0);
            self.send_frame(frame);
        }
    }

    fn on_scroll(&mut self, event: &ScrollEvent, window: &Window) {
        if !self.input_enabled {
            return;
        }
        self.update_mouse_position(
            f32::from(event.position.x),
            f32::from(event.position.y),
            window,
        );
        let dy = match event.delta {
            ScrollDelta::Lines(point) => point.y,
            ScrollDelta::Pixels(point) => f32::from(point.y) / 120.0,
        };
        let wheel = if dy > 0.0 {
            1
        } else if dy < 0.0 {
            -1
        } else {
            0
        };
        if wheel != 0 {
            let frame = self.mouse.frame(wheel);
            self.send_frame(frame);
        }
    }
}

impl Drop for VideoView {
    fn drop(&mut self) {
        self.release_all();
    }
}

impl Render for VideoView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for old in self.retired.drain(..) {
            cx.drop_image(old, Some(window));
        }
        self.report();

        let content = if let Some(image) = &self.latest {
            img(ImageSource::Render(image.clone()))
                .size_full()
                .object_fit(ObjectFit::Contain)
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

        let focus_handle = self.focus_handle.clone();
        let screen_label: Option<&str> = match self.screen_state {
            ScreenState::Live => None,
            ScreenState::NoSignal => Some("目标屏幕无信号 / 已息屏"),
            ScreenState::Blank => Some("目标屏幕已息屏"),
        };
        let overlay_label: Option<String> = self
            .device_message
            .clone()
            .or_else(|| screen_label.map(str::to_string));
        let mut root = div()
            .track_focus(&focus_handle)
            .size_full()
            .relative()
            .bg(gpui::black())
            .on_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                view.on_key_down(event, window, cx)
            }))
            .on_key_up(cx.listener(|view, event: &KeyUpEvent, _window, _cx| view.on_key_up(event)))
            .on_mouse_move(cx.listener(|view, event: &MouseMoveEvent, window, _cx| {
                view.on_mouse_move(event, window)
            }))
            .on_mouse_down(
                GpuiMouseButton::Left,
                cx.listener(|view, event: &MouseDownEvent, window, cx| {
                    view.on_mouse_button(GpuiMouseButton::Left, true, event, window, cx)
                }),
            )
            .on_mouse_down(
                GpuiMouseButton::Right,
                cx.listener(|view, event: &MouseDownEvent, window, cx| {
                    view.on_mouse_button(GpuiMouseButton::Right, true, event, window, cx)
                }),
            )
            .on_mouse_down(
                GpuiMouseButton::Middle,
                cx.listener(|view, event: &MouseDownEvent, window, cx| {
                    view.on_mouse_button(GpuiMouseButton::Middle, true, event, window, cx)
                }),
            )
            .on_mouse_up(
                GpuiMouseButton::Left,
                cx.listener(|view, event: &MouseUpEvent, window, _cx| {
                    view.on_mouse_up_button(GpuiMouseButton::Left, event, window)
                }),
            )
            .on_mouse_up(
                GpuiMouseButton::Right,
                cx.listener(|view, event: &MouseUpEvent, window, _cx| {
                    view.on_mouse_up_button(GpuiMouseButton::Right, event, window)
                }),
            )
            .on_mouse_up(
                GpuiMouseButton::Middle,
                cx.listener(|view, event: &MouseUpEvent, window, _cx| {
                    view.on_mouse_up_button(GpuiMouseButton::Middle, event, window)
                }),
            )
            .on_scroll(
                cx.listener(|view, event: &ScrollEvent, window, _cx| view.on_scroll(event, window)),
            )
            .child(content);
        if let Some(label) = overlay_label {
            root = root.child(
                div()
                    .absolute()
                    .left(px(12.0))
                    .bottom(px(12.0))
                    .px(px(10.0))
                    .py(px(4.0))
                    .rounded_md()
                    .bg(gpui::rgb(0x202020))
                    .text_color(gpui::white())
                    .text_sm()
                    .child(label),
            );
        }
        root
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct ContentRect {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

/// 计算 `ObjectFit::Contain` 下视频在窗口内的实际显示矩形。
fn content_rect(viewport_w: f32, viewport_h: f32, video_w: f32, video_h: f32) -> ContentRect {
    if !viewport_w.is_finite() || !viewport_h.is_finite() || viewport_w <= 0.0 || viewport_h <= 0.0
    {
        return ContentRect {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        };
    }
    if !video_w.is_finite() || !video_h.is_finite() || video_w <= 0.0 || video_h <= 0.0 {
        return ContentRect {
            x: 0.0,
            y: 0.0,
            width: viewport_w,
            height: viewport_h,
        };
    }
    let video_aspect = video_w / video_h;
    let view_aspect = viewport_w / viewport_h;
    if view_aspect > video_aspect {
        // 左右黑边
        let height = viewport_h;
        let width = height * video_aspect;
        ContentRect {
            x: (viewport_w - width) / 2.0,
            y: 0.0,
            width,
            height,
        }
    } else {
        // 上下黑边
        let width = viewport_w;
        let height = width / video_aspect;
        ContentRect {
            x: 0.0,
            y: (viewport_h - height) / 2.0,
            width,
            height,
        }
    }
}

fn to_modifiers(modifiers: &gpui::Modifiers) -> Modifiers {
    Modifiers {
        control: modifiers.control,
        alt: modifiers.alt,
        shift: modifiers.shift,
        platform: modifiers.platform,
    }
}

fn to_mouse_button(button: GpuiMouseButton) -> Option<MouseButton> {
    match button {
        GpuiMouseButton::Left => Some(MouseButton::Left),
        GpuiMouseButton::Right => Some(MouseButton::Right),
        GpuiMouseButton::Middle => Some(MouseButton::Middle),
        _ => None,
    }
}

/// 打开主窗口并开始消费视频/输入事件。
///
/// `hid` 为 None 时只显示视频，不注入输入。函数阻塞直到应用退出。
pub fn run(
    rx: Receiver<VideoEvent>,
    presence_rx: Receiver<DevicePresence>,
    hid: Option<HidHandle>,
) -> Result<()> {
    let open_error = Arc::new(std::sync::Mutex::new(None::<String>));
    let open_error_in = open_error.clone();
    let quitting = Arc::new(AtomicBool::new(false));
    application().run(move |cx: &mut App| {
        // 最后一个窗口关闭后退出，避免 GPUI 在无窗口状态下继续访问已销毁的 HWND。
        cx.on_window_closed(|cx, _window_id| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = Bounds::new(gpui::point(px(50.0), px(50.0)), size(px(1280.0), px(720.0)));
        match cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            move |window, cx| {
                // 拦截系统关闭：直接 quit，阻止 GPUI 在窗口销毁后继续访问 HWND。
                let quitting = quitting.clone();
                window.on_window_should_close(cx, move |_window, cx| {
                    if !quitting.swap(true, Ordering::Relaxed) {
                        cx.quit();
                    }
                    false
                });

                let view = cx.new(|cx| VideoView::new(rx, presence_rx, hid, cx));
                let handle = view.read(cx).focus_handle.clone();
                view.update(cx, |view, cx| {
                    let subscription = cx.on_blur(&handle, window, |view, _window, _cx| {
                        view.release_all();
                    });
                    view.subscriptions.push(subscription);
                });
                window.focus(&handle, cx);
                view
            },
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

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    #[test]
    fn content_rect_pillarbox() {
        let rect = content_rect(1400.0, 720.0, 1920.0, 1080.0);
        assert!(close(rect.x, 60.0));
        assert!(close(rect.y, 0.0));
        assert!(close(rect.width, 1280.0));
        assert!(close(rect.height, 720.0));
    }

    #[test]
    fn content_rect_letterbox() {
        let rect = content_rect(1280.0, 800.0, 1920.0, 1080.0);
        assert!(close(rect.x, 0.0));
        assert!(close(rect.y, 40.0));
        assert!(close(rect.width, 1280.0));
        assert!(close(rect.height, 720.0));
    }

    #[test]
    fn content_rect_exact_aspect() {
        let rect = content_rect(1280.0, 720.0, 1920.0, 1080.0);
        assert!(close(rect.x, 0.0));
        assert!(close(rect.y, 0.0));
        assert!(close(rect.width, 1280.0));
        assert!(close(rect.height, 720.0));
    }
}
