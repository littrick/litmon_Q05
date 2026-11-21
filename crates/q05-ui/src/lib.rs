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
    AnyElement, App, Bounds, Context, FocusHandle, ImageSource, IntoElement, KeyDownEvent,
    KeyUpEvent, MouseButton as GpuiMouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ObjectFit, Render, RenderImage, ScrollDelta, ScrollEvent, Subscription, Window, WindowBounds,
    WindowOptions, div, img, px, size,
};
use gpui_platform::application;
use image::{Frame as ImageFrame, ImageBuffer, Rgba};
use q05_hid::HidHandle;
use q05_input::{
    KeyboardState, Modifiers, MouseButton, MouseState, hid_usage_for_key,
    normalize_position_in_rect,
};
use q05_session::{AudioStatus, DevicePresence};
use q05_video::{Frame, ScreenState, VideoError, VideoEvent, VideoFormat};

const MOUSE_SEND_INTERVAL: Duration = Duration::from_millis(8);

/// 窗口最小宽度（逻辑像素）。
const MIN_WINDOW_WIDTH: f32 = 640.0;
/// 窗口最小高度；按当前视频格式（1080p，16:9）的比例计算。
/// 无视频时同样使用 16:9 回退。
const MIN_WINDOW_HEIGHT: f32 =
    MIN_WINDOW_WIDTH * VideoFormat::FHD30.height as f32 / VideoFormat::FHD30.width as f32;

// ==================== 右下角状态方块（HUD）可调参数 ====================
/// 背景色，格式 0xRRGGBBAA；当前为 50% 不透明黑色。
const HUD_BG_RGBA: u32 = 0x00000050;
/// 文字颜色，格式 0xRRGGBBAA。
const HUD_TEXT_RGBA: u32 = 0xFFFFFFFF;
/// HUD 小号字号（逻辑像素，默认）。
const HUD_FONT_SMALL: f32 = 10.5;
/// HUD 大号字号（逻辑像素）。
const HUD_FONT_LARGE: f32 = 16.0;
/// 行间距（逻辑像素）。
const HUD_GAP: f32 = 0.0;
/// 内边距 = 字号 × 该系数。
const HUD_PADDING_FACTOR: f32 = 0.6;
/// 距窗口右边缘/下边缘的距离（逻辑像素）。
const HUD_MARGIN: f32 = 30.0;
/// 圆角（逻辑像素）。
const HUD_RADIUS: f32 = 4.0;

/// 右键菜单字号（逻辑像素）。
const MENU_FONT_SIZE: f32 = 10.0;
/// 菜单文字行高系数。
const MENU_LINE_HEIGHT_FACTOR: f32 = 2.3;
/// 菜单宽度系数。
const MENU_LINE_WIDTH_FACTOR: f32 = 8.0;
/// 单个菜单项的行高（逻辑像素）。
const MENU_LINE_HEIGHT: f32 = MENU_FONT_SIZE * MENU_LINE_HEIGHT_FACTOR;
/// 右键菜单宽度（逻辑像素，由字号推导）。
const MENU_WIDTH: f32 = MENU_FONT_SIZE * MENU_LINE_WIDTH_FACTOR;
/// 菜单内边距（上下左右，逻辑像素）。
const MENU_PADDING: f32 = 0.0;
/// 菜单距窗口边缘的最小距离（逻辑像素）。
const MENU_MARGIN: f32 = 8.0;

/// HUD 尺寸档位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum HudSize {
    /// 小（默认）。
    #[default]
    Small,
    /// 大。
    Large,
}

impl HudSize {
    fn font_size(self) -> f32 {
        match self {
            Self::Small => HUD_FONT_SMALL,
            Self::Large => HUD_FONT_LARGE,
        }
    }

    /// 菜单里显示的动作文案（点击后切换到的尺寸）。
    fn menu_label(self) -> &'static str {
        match self {
            Self::Small => "放大",
            Self::Large => "缩小",
        }
    }

    /// 小 → 大 → 小。
    fn next(self) -> Self {
        match self {
            Self::Small => Self::Large,
            Self::Large => Self::Small,
        }
    }
}

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
    /// 最近一秒统计到的帧率。
    fps: f64,
    /// 已透传的坐标移动事件数。
    move_passthroughs: u64,
    /// 已透传的左键按下/释放事件数。
    left_passthroughs: u64,
    /// 右键菜单是否打开。
    menu_open: bool,
    /// 右键菜单位置（窗口逻辑坐标）。
    menu_pos: (f32, f32),
    /// 菜单关闭后的第一次左键释放不再透传。
    suppress_left_up: bool,
    width: u32,
    height: u32,
    focus_handle: FocusHandle,
    subscriptions: Vec<Subscription>,
    hid: Option<HidHandle>,
    keyboard: KeyboardState,
    mouse: MouseState,
    input_enabled: bool,
    input_requested: bool,
    last_mouse_send: Instant,
    mouse_logged: bool,
    log_all_mouse: bool,
    screen_state: ScreenState,
    device_present: bool,
    presence_seen: bool,
    device_message: Option<String>,
    audio_status: AudioStatus,
    hud_size: HudSize,
    hud_top_right: bool,
    always_on_top: bool,
}

impl VideoView {
    fn new(
        rx: Receiver<VideoEvent>,
        presence_rx: Receiver<DevicePresence>,
        hid_rx: Receiver<Option<HidHandle>>,
        audio_rx: Receiver<AudioStatus>,
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

        cx.spawn(async move |this, cx| {
            while let Ok(hid) = hid_rx.recv().await {
                let alive = this
                    .update(cx, |view, cx| {
                        view.handle_hid(hid);
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
            while let Ok(status) = audio_rx.recv().await {
                let alive = this
                    .update(cx, |view, cx| {
                        view.audio_status = status;
                        cx.notify();
                    })
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();

        let input_enabled = false;
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
            fps: 0.0,
            move_passthroughs: 0,
            left_passthroughs: 0,
            menu_open: false,
            menu_pos: (8.0, 8.0),
            suppress_left_up: false,
            width: 0,
            height: 0,
            focus_handle: cx.focus_handle(),
            subscriptions: Vec::new(),
            hid: None,
            keyboard: KeyboardState::new(),
            mouse: MouseState::new(),
            input_enabled,
            input_requested: true,
            last_mouse_send: Instant::now() - MOUSE_SEND_INTERVAL,
            mouse_logged: false,
            log_all_mouse: std::env::var_os("Q05_MOUSE_LOG").is_some(),
            screen_state: ScreenState::Live,
            device_present: false,
            presence_seen: false,
            device_message: None,
            audio_status: AudioStatus::Stopped,
            hud_size: HudSize::Small,
            hud_top_right: false,
            always_on_top: false,
        }
    }

    fn handle_event(&mut self, event: VideoEvent) {
        match event {
            VideoEvent::Frame(frame) => self.set_frame(frame),
            VideoEvent::Error(err) => {
                tracing::error!(error = %err, "video capture error");
                // 没有画面就不注入输入；HID 由 supervisor 在插拔时管理。
                self.clear_frame();
                self.input_enabled = false;
                self.screen_state = ScreenState::Live;
                self.status = format!("视频错误：{err}\n请检查相机占用/设备连接");
            }
            VideoEvent::ScreenState(state) => {
                self.screen_state = state;
                tracing::info!(screen_state = ?state, "target screen state");
            }
            VideoEvent::Stopped => {
                tracing::info!("video capture stopped");
                self.clear_frame();
                self.input_enabled = false;
                self.status = "视频已停止，等待设备重新连接…".to_string();
            }
        }
    }

    fn handle_presence(&mut self, presence: DevicePresence) {
        let connected = presence.connected();
        if !self.presence_seen {
            self.presence_seen = true;
            self.device_present = connected;
            if !connected {
                self.release_all();
                self.input_enabled = false;
                self.hid = None;
                self.clear_frame();
                self.device_message = Some("Q0.5 设备未连接".to_string());
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
            self.clear_frame();
            self.screen_state = ScreenState::Live;
            self.device_message = Some("Q0.5 设备已断开，等待重新插入…".to_string());
            tracing::warn!(?presence, "device disconnected");
        } else if !was_connected && connected {
            self.device_message = None;
            self.screen_state = ScreenState::Live;
            self.status = "Q0.5 已重新连接，正在恢复视频…".to_string();
            tracing::info!(?presence, "device reconnected; rebuilding media");
        }
    }

    fn handle_hid(&mut self, hid: Option<HidHandle>) {
        match hid {
            Some(hid) => {
                self.hid = Some(hid);
                self.input_enabled = self.input_requested;
                tracing::info!(input_requested = self.input_requested, "input HID ready");
            }
            None => {
                self.release_all();
                self.hid = None;
                self.input_enabled = false;
                tracing::info!("input HID removed");
            }
        }
    }

    fn clear_frame(&mut self) {
        if let Some(old) = self.latest.take() {
            self.retired.push(old);
        }
        self.width = 0;
        self.height = 0;
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
        if self.input_requested && self.hid.is_some() {
            self.input_enabled = true;
        }
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
        self.fps = fps;
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

    /// 视频在窗口内的实际显示矩形。
    ///
    /// 鼠标映射与渲染共用同一份计算，避免 resize 后两者不一致。
    fn video_rect(&self, window: &Window) -> ContentRect {
        let (video_w, video_h) = if self.width > 0 && self.height > 0 {
            (self.width as f32, self.height as f32)
        } else {
            (1920.0, 1080.0)
        };
        self.content_rect_for(window, video_w, video_h)
    }

    fn content_rect_for(&self, window: &Window, video_w: f32, video_h: f32) -> ContentRect {
        let size = window.viewport_size();
        content_rect(
            f32::from(size.width),
            f32::from(size.height),
            video_w,
            video_h,
        )
    }

    fn update_mouse_position(&mut self, x: f32, y: f32, window: &Window) {
        let rect = self.video_rect(window);
        let viewport = window.viewport_size();
        let (nx, ny) = normalize_position_in_rect(x, y, rect.x, rect.y, rect.width, rect.height);
        self.mouse.set_position(nx, ny);
        if !self.mouse_logged || self.log_all_mouse {
            self.mouse_logged = true;
            tracing::info!(
                local_x = x,
                local_y = y,
                viewport_w = f32::from(viewport.width),
                viewport_h = f32::from(viewport.height),
                video_w = self.width,
                video_h = self.height,
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
        // 菜单打开时 Esc 只关闭菜单，不透传给目标。
        if self.menu_open && event.keystroke.key.eq_ignore_ascii_case("escape") {
            self.menu_open = false;
            cx.notify();
            return;
        }
        // F11 切换全屏，不透传给目标。
        if event.keystroke.key.eq_ignore_ascii_case("f11") {
            window.toggle_fullscreen();
            cx.notify();
            return;
        }
        let modifiers = to_modifiers(&event.keystroke.modifiers);
        // 安全开关：Ctrl+Alt+Q。
        if modifiers.control && modifiers.alt && event.keystroke.key.eq_ignore_ascii_case("q") {
            self.input_requested = !self.input_requested;
            self.input_enabled = self.input_requested && self.hid.is_some();
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
        if !self.input_enabled || self.menu_open {
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
            self.move_passthroughs += 1;
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
        if !self.input_enabled || self.menu_open {
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
            if button == MouseButton::Left && pressed {
                self.left_passthroughs += 1;
            }
        }
    }

    fn on_mouse_up_button(
        &mut self,
        button: GpuiMouseButton,
        event: &MouseUpEvent,
        window: &Window,
    ) {
        if !self.input_enabled || self.menu_open {
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

    /// 在右下角状态方块上右键打开本地菜单。
    fn open_context_menu(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) {
        // 先记录点击位置，渲染时再按真实菜单高度钳制，避免增删菜单项后越界。
        self.menu_open = true;
        self.menu_pos = (f32::from(event.position.x), f32::from(event.position.y));
        tracing::info!(
            x = self.menu_pos.0,
            y = self.menu_pos.1,
            "context menu opened"
        );
        cx.notify();
    }

    fn on_scroll(&mut self, event: &ScrollEvent, window: &Window) {
        if !self.input_enabled || self.menu_open {
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

    /// 右下角状态方块：分辨率、FPS、坐标与按键透传计数。
    fn render_hud(&self, cx: &mut Context<Self>) -> AnyElement {
        let font = self.hud_size.font_size();
        let device = if !self.presence_seen {
            "设备检测中…"
        } else if self.device_present {
            "设备已连接"
        } else {
            "设备未连接"
        };
        let video = if self.width > 0 && self.height > 0 {
            format!("{}×{} · {:.0} fps", self.width, self.height, self.fps)
        } else {
            "视频未连接".to_string()
        };
        let audio = match self.audio_status {
            AudioStatus::Stopped => "音频未连接",
            AudioStatus::Starting => "音频启动中…",
            AudioStatus::Running => "音频已连接",
        };
        let mut hud = div()
            .id("status-hud")
            .absolute()
            .right(px(HUD_MARGIN))
            .rounded(px(HUD_RADIUS))
            .bg(gpui::rgba(HUD_BG_RGBA))
            .text_color(gpui::rgba(HUD_TEXT_RGBA))
            .text_size(px(font))
            .p(px(font * HUD_PADDING_FACTOR))
            .flex()
            .flex_col()
            .gap(px(HUD_GAP))
            .child(device)
            .child(video)
            .child(audio)
            .child("右键打开菜单")
            .on_mouse_down(
                GpuiMouseButton::Right,
                cx.listener(|view, event: &MouseDownEvent, _window, cx| {
                    view.open_context_menu(event, cx);
                    cx.stop_propagation();
                }),
            );
        hud = if self.hud_top_right {
            hud.top(px(HUD_MARGIN))
        } else {
            hud.bottom(px(HUD_MARGIN))
        };
        hud.into_any_element()
    }

    /// 单个菜单项。
    fn menu_item(
        label: &str,
        on_select: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .px(px(10.0))
            .rounded_sm()
            .cursor_pointer()
            .hover(|style| style.bg(gpui::rgb(0x444444)))
            .on_mouse_down(
                GpuiMouseButton::Left,
                cx.listener(move |view, _event: &MouseDownEvent, window, cx| {
                    view.menu_open = false;
                    cx.stop_propagation();
                    on_select(view, window, cx);
                }),
            )
            .child(label.to_string())
            .into_any_element()
    }

    /// 右键下拉菜单。
    ///
    /// 高度由实际菜单项数计算，增删菜单项不需要再同步常量。
    fn render_menu(
        &self,
        viewport_w: f32,
        viewport_h: f32,
        fullscreen: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut items: Vec<AnyElement> = Vec::new();
        items.push(Self::menu_item(
            &format!("HUD{}", self.hud_size.menu_label()),
            |view, _window, cx| {
                view.hud_size = view.hud_size.next();
                cx.notify();
            },
            cx,
        ));
        items.push(Self::menu_item(
            if self.hud_top_right {
                "移到右下"
            } else {
                "移到右上"
            },
            |view, _window, cx| {
                view.hud_top_right = !view.hud_top_right;
                cx.notify();
            },
            cx,
        ));
        items.push(Self::menu_item(
            if fullscreen { "退出全屏" } else { "全屏" },
            |_view, window, cx| {
                window.toggle_fullscreen();
                cx.notify();
            },
            cx,
        ));
        items.push(Self::menu_item(
            topmost_label(self.always_on_top),
            |view, window, cx| {
                view.always_on_top = !view.always_on_top;
                set_always_on_top(window, view.always_on_top);
                cx.notify();
            },
            cx,
        ));
        items.push(Self::menu_item(
            "关闭窗口",
            |_view, _window, cx| cx.defer(|cx| cx.quit()),
            cx,
        ));

        let full_height = menu_height(items.len());
        let max_height = (viewport_h - MENU_MARGIN * 2.0).max(MENU_LINE_HEIGHT);
        let menu_height = full_height.min(max_height);
        let (x, y) = clamp_menu_pos(
            self.menu_pos.0,
            self.menu_pos.1,
            viewport_w,
            viewport_h,
            menu_height,
        );
        div()
            .id("context-menu")
            .absolute()
            .left(px(x))
            .top(px(y))
            .w(px(MENU_WIDTH))
            .max_h(px(max_height))
            .overflow_y_scroll()
            .whitespace_nowrap()
            .rounded_md()
            .bg(gpui::rgb(0x2A2A2A))
            // .border_1()
            // .border_color(gpui::rgb(0x666666))
            .p(px(MENU_PADDING))
            .text_color(gpui::white())
            .text_size(px(MENU_FONT_SIZE))
            .line_height(px(MENU_LINE_HEIGHT))
            .on_mouse_down(
                GpuiMouseButton::Left,
                cx.listener(|_view, _event: &MouseDownEvent, _window, cx| {
                    // 点在菜单空白处：只消费事件，不透传给目标。
                    cx.stop_propagation();
                }),
            )
            .children(items)
            .into_any_element()
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
            video_content(self.video_rect(window), image.clone())
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
                    // 菜单打开时，左键只关闭菜单，不透传给目标。
                    if view.menu_open {
                        view.menu_open = false;
                        view.suppress_left_up = true;
                        cx.notify();
                        return;
                    }
                    view.on_mouse_button(GpuiMouseButton::Left, true, event, window, cx)
                }),
            )
            .on_mouse_down(
                GpuiMouseButton::Right,
                cx.listener(|view, event: &MouseDownEvent, window, cx| {
                    // 状态方块自己的右键监听会拦截并弹菜单；其余区域照常透传。
                    if view.menu_open {
                        view.menu_open = false;
                        cx.notify();
                        return;
                    }
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
                    if view.suppress_left_up {
                        view.suppress_left_up = false;
                        return;
                    }
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
        let hud = self.render_hud(cx);
        let menu = self.menu_open.then(|| {
            let size = window.viewport_size();
            self.render_menu(
                f32::from(size.width),
                f32::from(size.height),
                window.is_fullscreen(),
                cx,
            )
        });
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
        root = root.child(hud);
        if let Some(menu) = menu {
            root = root.child(menu);
        }
        root
    }
}

/// 按 `content_rect` 显式绘制视频。
///
/// 不依赖 `Img` 自身的 `ObjectFit::Contain`：GPUI 的 `Img` 会写入内在宽高比，
/// 在宽窗口下可能把元素撑得比窗口更高，导致实际绘制区域与鼠标映射不一致。
fn video_content(rect: ContentRect, image: Arc<RenderImage>) -> AnyElement {
    div()
        .absolute()
        .left(px(rect.x))
        .top(px(rect.y))
        .w(px(rect.width.max(0.0)))
        .h(px(rect.height.max(0.0)))
        .overflow_hidden()
        .child(
            img(ImageSource::Render(image))
                .size_full()
                .object_fit(ObjectFit::Fill),
        )
        .into_any_element()
}

/// 菜单总高度：所有菜单项行高 + 上下内边距。
fn menu_height(item_count: usize) -> f32 {
    MENU_LINE_HEIGHT * item_count as f32 + MENU_PADDING * 2.0
}

/// 把菜单左上角限制在窗口内，保证整块菜单可见。
fn clamp_menu_pos(
    x: f32,
    y: f32,
    viewport_w: f32,
    viewport_h: f32,
    menu_height: f32,
) -> (f32, f32) {
    let max_x = (viewport_w - MENU_WIDTH - MENU_MARGIN).max(MENU_MARGIN);
    let max_y = (viewport_h - menu_height - MENU_MARGIN).max(MENU_MARGIN);
    (x.clamp(MENU_MARGIN, max_x), y.clamp(MENU_MARGIN, max_y))
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
    hid_rx: Receiver<Option<HidHandle>>,
    audio_rx: Receiver<AudioStatus>,
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
                window_min_size: Some(size(px(MIN_WINDOW_WIDTH), px(MIN_WINDOW_HEIGHT))),
                ..Default::default()
            },
            move |window, cx| {
                #[cfg(windows)]
                apply_window_icon(window);

                // 拦截系统关闭：直接 quit，阻止 GPUI 在窗口销毁后继续访问 HWND。
                let quitting = quitting.clone();
                window.on_window_should_close(cx, move |_window, cx| {
                    if !quitting.swap(true, Ordering::Relaxed) {
                        cx.quit();
                    }
                    false
                });

                let view = cx.new(|cx| VideoView::new(rx, presence_rx, hid_rx, audio_rx, cx));
                let handle = view.read(cx).focus_handle.clone();
                view.update(cx, |view, cx| {
                    let subscription = cx.on_blur(&handle, window, |view, _window, _cx| {
                        view.menu_open = false;
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

/// 菜单里的置顶文案（显示点击后的动作）。
fn topmost_label(always_on_top: bool) -> &'static str {
    if always_on_top {
        "取消置顶"
    } else {
        "置顶"
    }
}

/// 切换窗口最上层。
#[cfg(windows)]
fn set_always_on_top(window: &Window, always_on_top: bool) {
    use raw_window_handle::RawWindowHandle;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos,
    };

    let Ok(handle) = raw_window_handle::HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(hwnd) = handle.as_raw() else {
        return;
    };
    let hwnd = HWND(hwnd.hwnd.get() as *mut core::ffi::c_void);
    let after = if always_on_top {
        HWND_TOPMOST
    } else {
        HWND_NOTOPMOST
    };
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(after),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

#[cfg(not(windows))]
fn set_always_on_top(_window: &Window, _always_on_top: bool) {}

/// 显式设置窗口大/小图标，确保任务栏使用嵌入的 logo（资源 ID 1）。
#[cfg(windows)]
fn apply_window_icon(window: &Window) {
    use raw_window_handle::RawWindowHandle;
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        GCLP_HICON, GCLP_HICONSM, GetSystemMetrics, ICON_BIG, ICON_SMALL, ICON_SMALL2, IMAGE_ICON,
        LR_DEFAULTCOLOR, LoadImageW, SM_CXICON, SM_CXSMICON, SM_CYICON, SM_CYSMICON, SendMessageW,
        SetClassLongPtrW, WM_SETICON,
    };

    let Ok(handle) = raw_window_handle::HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::Win32(hwnd) = handle.as_raw() else {
        return;
    };
    let hwnd = HWND(hwnd.hwnd.get() as *mut core::ffi::c_void);

    let Ok(module) = (unsafe { GetModuleHandleW(None) }) else {
        return;
    };
    let hinstance = HINSTANCE(module.0);
    let name = windows::core::PCWSTR(std::ptr::without_provenance(1));

    let cx = unsafe { GetSystemMetrics(SM_CXICON) };
    let cy = unsafe { GetSystemMetrics(SM_CYICON) };
    let scx = unsafe { GetSystemMetrics(SM_CXSMICON) };
    let scy = unsafe { GetSystemMetrics(SM_CYSMICON) };

    let big = unsafe { LoadImageW(Some(hinstance), name, IMAGE_ICON, cx, cy, LR_DEFAULTCOLOR) };
    if let Ok(big) = big {
        unsafe {
            SendMessageW(
                hwnd,
                WM_SETICON,
                Some(WPARAM(ICON_BIG as usize)),
                Some(LPARAM(big.0 as isize)),
            );
            SetClassLongPtrW(hwnd, GCLP_HICON, big.0 as isize);
        }
    }

    let small = unsafe { LoadImageW(Some(hinstance), name, IMAGE_ICON, scx, scy, LR_DEFAULTCOLOR) };
    if let Ok(small) = small {
        unsafe {
            SendMessageW(
                hwnd,
                WM_SETICON,
                Some(WPARAM(ICON_SMALL as usize)),
                Some(LPARAM(small.0 as isize)),
            );
            SendMessageW(
                hwnd,
                WM_SETICON,
                Some(WPARAM(ICON_SMALL2 as usize)),
                Some(LPARAM(small.0 as isize)),
            );
            SetClassLongPtrW(hwnd, GCLP_HICONSM, small.0 as isize);
        }
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
    fn topmost_label_shows_next_action() {
        assert_eq!(topmost_label(false), "置顶");
        assert_eq!(topmost_label(true), "取消置顶");
    }

    #[test]
    fn hud_size_presets_are_ordered_and_cycle() {
        assert!(HudSize::Small.font_size() < HudSize::Large.font_size());
        assert_eq!(HudSize::default(), HudSize::Small);
        assert_eq!(HudSize::Small.next(), HudSize::Large);
        assert_eq!(HudSize::Large.next(), HudSize::Small);
    }

    #[test]
    fn menu_position_is_clamped_to_window() {
        let two = menu_height(2);
        assert_eq!(
            clamp_menu_pos(300.0, 200.0, 1000.0, 800.0, two),
            (300.0, 200.0)
        );
        assert_eq!(
            clamp_menu_pos(990.0, 790.0, 1000.0, 800.0, two),
            (1000.0 - MENU_WIDTH - MENU_MARGIN, 800.0 - two - MENU_MARGIN)
        );
        // 菜单项增加时，钳制高度随之变化，不会越界。
        let five = menu_height(5);
        assert_eq!(
            clamp_menu_pos(990.0, 790.0, 1000.0, 800.0, five).1,
            800.0 - five - MENU_MARGIN
        );
        assert_eq!(
            clamp_menu_pos(0.0, 0.0, 100.0, 100.0, two),
            (MENU_MARGIN, MENU_MARGIN)
        );
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
