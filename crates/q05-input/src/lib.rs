//! Q0.5 输入状态机、HID usage/坐标映射。
//!
//! LF0 目标机实测：
//! - 键槽是 **HID usage**（0x04='a'，0x41=F8，Enter=0x28）；
//! - 鼠标 x/y 的有效范围是 **0..32767（15-bit）**（16383/16384 ≈ 屏幕中心，32767 = 右下角）；
//! - 修饰键位图是标准 HID 语义（0x01 LCTRL / 0x02 LSHIFT / 0x04 LALT / 0x08 LWIN）。

use q05_proto::Frame;

/// Q0.5 绝对坐标有效范围上限（目标端 HID 是 15-bit）。
pub const ABS_MAX: u16 = 32767;

/// 鼠标按键。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    /// 左键。
    Left,
    /// 右键。
    Right,
    /// 中键。
    Middle,
}

/// 与 GPUI 解耦的修饰键状态。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    /// Ctrl。
    pub control: bool,
    /// Alt。
    pub alt: bool,
    /// Shift。
    pub shift: bool,
    /// Windows / Super。
    pub platform: bool,
}

impl Modifiers {
    /// 转成 HID 修饰键位图。
    pub fn hid_bits(self) -> u8 {
        let mut bits = 0;
        if self.control {
            bits |= 0x01;
        }
        if self.shift {
            bits |= 0x02;
        }
        if self.alt {
            bits |= 0x04;
        }
        if self.platform {
            bits |= 0x08;
        }
        bits
    }
}

/// GPUI key 名称 → HID usage。
///
/// 返回 `None` 表示修饰键、未知键或不支持的特殊键。
pub fn hid_usage_for_key(key: &str) -> Option<u8> {
    let lower = key.to_ascii_lowercase();
    let k = lower.as_str();

    // 字母
    let bytes = lower.as_bytes();
    if bytes.len() == 1 && bytes[0].is_ascii_lowercase() {
        return Some(0x04 + (bytes[0] - b'a'));
    }
    // 数字
    if lower.len() == 1
        && let Some(d) = lower.chars().next().and_then(|c| c.to_digit(10))
    {
        return Some(if d == 0 { 0x27 } else { 0x1E + d as u8 - 1 });
    }

    let usage = match k {
        "enter" | "return" => 0x28,
        "escape" | "esc" => 0x29,
        "backspace" => 0x2A,
        "tab" => 0x2B,
        "space" => 0x2C,
        "-" | "minus" => 0x2D,
        "=" | "equal" => 0x2E,
        "[" | "leftbracket" | "left_bracket" => 0x2F,
        "]" | "rightbracket" | "right_bracket" => 0x30,
        "\\" | "backslash" => 0x31,
        ";" | "semicolon" => 0x33,
        "'" | "quote" => 0x34,
        "`" | "grave" => 0x35,
        "," | "comma" => 0x36,
        "." | "period" => 0x37,
        "/" | "slash" => 0x38,
        "capslock" | "caps_lock" => 0x39,
        "f1" => 0x3A,
        "f2" => 0x3B,
        "f3" => 0x3C,
        "f4" => 0x3D,
        "f5" => 0x3E,
        "f6" => 0x3F,
        "f7" => 0x40,
        "f8" => 0x41,
        "f9" => 0x42,
        "f10" => 0x43,
        "f11" => 0x44,
        "f12" => 0x45,
        "printscreen" | "print_screen" => 0x46,
        "scrolllock" | "scroll_lock" => 0x47,
        "pause" => 0x48,
        "insert" => 0x49,
        "home" => 0x4A,
        "pageup" | "page_up" => 0x4B,
        "delete" | "del" => 0x4C,
        "end" => 0x4D,
        "pagedown" | "page_down" => 0x4E,
        "right" | "arrowright" | "arrow_right" => 0x4F,
        "left" | "arrowleft" | "arrow_left" => 0x50,
        "down" | "arrowdown" | "arrow_down" => 0x51,
        "up" | "arrowup" | "arrow_up" => 0x52,
        "numlock" | "num_lock" => 0x53,
        "menu" | "contextmenu" | "context_menu" => 0x65,
        // Shift 后的符号（某些后端会把 key 直接给成符号）
        "!" => 0x1E,
        "@" => 0x1F,
        "#" => 0x20,
        "$" => 0x21,
        "%" => 0x22,
        "^" => 0x23,
        "&" => 0x24,
        "*" => 0x25,
        "(" => 0x26,
        ")" => 0x27,
        "_" => 0x2D,
        "+" => 0x2E,
        "{" => 0x2F,
        "}" => 0x30,
        "|" => 0x31,
        ":" => 0x33,
        "\"" => 0x34,
        "~" => 0x35,
        "<" => 0x36,
        ">" => 0x37,
        "?" => 0x38,
        _ => return None,
    };
    Some(usage)
}

/// HID usage → 可读名称（HUD 显示用）。
///
/// 未知 usage 返回 `0xNN`，保证任何输入都有可读文本。
pub fn hid_usage_label(usage: u8) -> String {
    match usage {
        // 字母 a..z → A..Z
        0x04..=0x1D => ((b'A' + (usage - 0x04)) as char).to_string(),
        // 数字 1..9、0
        0x1E..=0x26 => (usage - 0x1E + 1).to_string(),
        0x27 => "0".to_string(),
        0x28 => "Enter".to_string(),
        0x29 => "Esc".to_string(),
        0x2A => "Backspace".to_string(),
        0x2B => "Tab".to_string(),
        0x2C => "Space".to_string(),
        0x2D => "-".to_string(),
        0x2E => "=".to_string(),
        0x2F => "[".to_string(),
        0x30 => "]".to_string(),
        0x31 => "\\".to_string(),
        0x33 => ";".to_string(),
        0x34 => "'".to_string(),
        0x35 => "`".to_string(),
        0x36 => ",".to_string(),
        0x37 => ".".to_string(),
        0x38 => "/".to_string(),
        0x39 => "CapsLock".to_string(),
        // F1..F12
        0x3A..=0x45 => format!("F{}", usage - 0x3A + 1),
        0x46 => "PrintScreen".to_string(),
        0x47 => "ScrollLock".to_string(),
        0x48 => "Pause".to_string(),
        0x49 => "Insert".to_string(),
        0x4A => "Home".to_string(),
        0x4B => "PageUp".to_string(),
        0x4C => "Delete".to_string(),
        0x4D => "End".to_string(),
        0x4E => "PageDown".to_string(),
        0x4F => "Right".to_string(),
        0x50 => "Left".to_string(),
        0x51 => "Down".to_string(),
        0x52 => "Up".to_string(),
        0x53 => "NumLock".to_string(),
        0x65 => "Menu".to_string(),
        // 修饰键 usage（KeyboardState 不存储，但保留映射便于调试）。
        0xE0 => "LCtrl".to_string(),
        0xE1 => "LShift".to_string(),
        0xE2 => "LAlt".to_string(),
        0xE3 => "LWin".to_string(),
        0xE4 => "RCtrl".to_string(),
        0xE5 => "RShift".to_string(),
        0xE6 => "RAlt".to_string(),
        0xE7 => "RWin".to_string(),
        _ => format!("0x{usage:02X}"),
    }
}

/// 当前按下键的显示文本（HUD 用）。
///
/// 修饰键按 Ctrl、Alt、Shift、Win 的顺序，普通键按按下顺序，统一用 `+` 连接；
/// 没有任何按键时返回 `None`。
pub fn pressed_keys_label(modifiers: u8, pressed: &[u8]) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if modifiers & 0x01 != 0 {
        parts.push("Ctrl".to_string());
    }
    if modifiers & 0x04 != 0 {
        parts.push("Alt".to_string());
    }
    if modifiers & 0x02 != 0 {
        parts.push("Shift".to_string());
    }
    if modifiers & 0x08 != 0 {
        parts.push("Win".to_string());
    }
    parts.extend(pressed.iter().map(|&usage| hid_usage_label(usage)));
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("+"))
    }
}

/// 把窗口内坐标转成 Q0.5 的 0..32767 归一化坐标（内容区从 0,0 开始）。
pub fn normalize_position(x: f32, y: f32, width: f32, height: f32) -> (u16, u16) {
    normalize_position_in_rect(x, y, 0.0, 0.0, width, height)
}

/// 把窗口内坐标转成 0..32767 归一化坐标，内容区可以带 letterbox 偏移。
///
/// Q0.5 视频用 `ObjectFit::Contain` 显示时，窗口内容区可能不是 16:9，
/// 实际画面位于 `[rect_x, rect_x+rect_w] × [rect_y, rect_y+rect_h]`。
pub fn normalize_position_in_rect(
    x: f32,
    y: f32,
    rect_x: f32,
    rect_y: f32,
    rect_w: f32,
    rect_h: f32,
) -> (u16, u16) {
    if !rect_w.is_finite()
        || !rect_h.is_finite()
        || !rect_x.is_finite()
        || !rect_y.is_finite()
        || rect_w <= 0.0
        || rect_h <= 0.0
    {
        return (0, 0);
    }
    let nx = ((x - rect_x) / rect_w).clamp(0.0, 1.0);
    let ny = ((y - rect_y) / rect_h).clamp(0.0, 1.0);
    (
        (nx * ABS_MAX as f32).round() as u16,
        (ny * ABS_MAX as f32).round() as u16,
    )
}

/// 键盘状态：最多 6 个同时按下的 HID usage + 修饰键位图。
#[derive(Debug, Default, Clone)]
pub struct KeyboardState {
    pressed: Vec<u8>,
    modifiers: u8,
}

impl KeyboardState {
    /// 新建空状态。
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前修饰键位图。
    pub fn modifiers(&self) -> u8 {
        self.modifiers
    }

    /// 当前按下的键槽。
    pub fn pressed(&self) -> &[u8] {
        &self.pressed
    }

    fn frame(&self) -> Frame {
        let mut keys = [0u8; 6];
        for (dst, src) in keys.iter_mut().zip(self.pressed.iter()) {
            *dst = *src;
        }
        Frame::keyboard(self.modifiers, keys)
    }

    /// 按下键；返回需要发送的报告（无变化时返回 None）。
    pub fn key_down(&mut self, usage: u8, modifiers: Modifiers) -> Option<Frame> {
        let bits = modifiers.hid_bits();
        if self.pressed.contains(&usage) {
            if self.modifiers != bits {
                self.modifiers = bits;
                return Some(self.frame());
            }
            return None;
        }
        if self.pressed.len() >= 6 {
            tracing::warn!("more than 6 keys pressed; dropping {usage:#04x}");
            return None;
        }
        self.pressed.push(usage);
        self.modifiers = bits;
        Some(self.frame())
    }

    /// 抬起键；返回需要发送的报告。
    pub fn key_up(&mut self, usage: u8, modifiers: Modifiers) -> Option<Frame> {
        let bits = modifiers.hid_bits();
        let removed = self.pressed.iter().position(|&k| k == usage);
        let had = removed.is_some();
        if let Some(index) = removed {
            self.pressed.remove(index);
        }
        if had || self.modifiers != bits {
            self.modifiers = bits;
            Some(self.frame())
        } else {
            None
        }
    }

    /// 只更新修饰键（例如单独按/放 Shift）；返回需要发送的报告。
    pub fn modifiers_changed(&mut self, modifiers: Modifiers) -> Option<Frame> {
        let bits = modifiers.hid_bits();
        if self.modifiers == bits {
            return None;
        }
        self.modifiers = bits;
        Some(self.frame())
    }

    /// 释放所有键并清空修饰键。
    pub fn release_all(&mut self) -> Option<Frame> {
        if self.pressed.is_empty() && self.modifiers == 0 {
            return None;
        }
        self.pressed.clear();
        self.modifiers = 0;
        Some(Frame::keyboard_release())
    }
}

/// 鼠标状态与绝对坐标。
#[derive(Debug, Default, Clone, Copy)]
pub struct MouseState {
    x: u16,
    y: u16,
    left: bool,
    right: bool,
    middle: bool,
}

impl MouseState {
    /// 新建状态（左上角、无按键）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前归一化坐标。
    pub fn position(&self) -> (u16, u16) {
        (self.x, self.y)
    }

    /// 设置归一化坐标。
    pub fn set_position(&mut self, x: u16, y: u16) {
        self.x = x;
        self.y = y;
    }

    /// 指定按键当前是否按下。
    pub fn is_pressed(&self, button: MouseButton) -> bool {
        match button {
            MouseButton::Left => self.left,
            MouseButton::Right => self.right,
            MouseButton::Middle => self.middle,
        }
    }

    /// 设置按键状态；返回是否变化。
    pub fn set_button(&mut self, button: MouseButton, pressed: bool) -> bool {
        let slot = match button {
            MouseButton::Left => &mut self.left,
            MouseButton::Right => &mut self.right,
            MouseButton::Middle => &mut self.middle,
        };
        if *slot == pressed {
            return false;
        }
        *slot = pressed;
        true
    }

    /// 当前按键状态。
    pub fn button(&self, button: MouseButton) -> bool {
        match button {
            MouseButton::Left => self.left,
            MouseButton::Right => self.right,
            MouseButton::Middle => self.middle,
        }
    }

    /// 生成一帧鼠标报告。
    pub fn frame(&self, wheel: i8) -> Frame {
        Frame::mouse_abs(self.x, self.y, self.left, self.right, self.middle, wheel)
    }

    /// 松开所有按键（保持当前位置）。
    pub fn release_buttons(&mut self) -> Frame {
        self.left = false;
        self.right = false;
        self.middle = false;
        self.frame(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_letters_digits_and_named_keys() {
        assert_eq!(hid_usage_for_key("a"), Some(0x04));
        assert_eq!(hid_usage_for_key("z"), Some(0x1D));
        assert_eq!(hid_usage_for_key("1"), Some(0x1E));
        assert_eq!(hid_usage_for_key("0"), Some(0x27));
        assert_eq!(hid_usage_for_key("enter"), Some(0x28));
        assert_eq!(hid_usage_for_key("left"), Some(0x50));
        assert_eq!(hid_usage_for_key("f12"), Some(0x45));
        assert_eq!(hid_usage_for_key("shift"), None);
        assert_eq!(hid_usage_for_key("unknown-key"), None);
    }

    #[test]
    fn modifier_bits() {
        let m = Modifiers {
            control: true,
            alt: true,
            shift: true,
            platform: true,
        };
        assert_eq!(m.hid_bits(), 0x0F);
        assert_eq!(Modifiers::default().hid_bits(), 0);
    }

    #[test]
    fn usage_labels_cover_common_keys() {
        assert_eq!(hid_usage_label(0x04), "A");
        assert_eq!(hid_usage_label(0x1D), "Z");
        assert_eq!(hid_usage_label(0x1E), "1");
        assert_eq!(hid_usage_label(0x27), "0");
        assert_eq!(hid_usage_label(0x28), "Enter");
        assert_eq!(hid_usage_label(0x29), "Esc");
        assert_eq!(hid_usage_label(0x2C), "Space");
        assert_eq!(hid_usage_label(0x3A), "F1");
        assert_eq!(hid_usage_label(0x45), "F12");
        assert_eq!(hid_usage_label(0x4F), "Right");
        assert_eq!(hid_usage_label(0xE0), "LCtrl");
        // 未知 usage 退化为十六进制，仍可读。
        assert_eq!(hid_usage_label(0xAA), "0xAA");
    }

    #[test]
    fn pressed_keys_label_orders_modifiers_and_keys() {
        // 无按键：不显示。
        assert_eq!(pressed_keys_label(0, &[]), None);
        // 单键。
        assert_eq!(pressed_keys_label(0, &[0x04]), Some("A".to_string()));
        // 修饰键按 Ctrl、Alt、Shift、Win 排序，普通键保持按下顺序。
        assert_eq!(
            pressed_keys_label(0x0F, &[0x05, 0x06]),
            Some("Ctrl+Alt+Shift+Win+B+C".to_string())
        );
        // 只有修饰键。
        assert_eq!(pressed_keys_label(0x01, &[]), Some("Ctrl".to_string()));
    }

    #[test]
    fn normalize_center_and_edges() {
        assert_eq!(normalize_position(0.0, 0.0, 1920.0, 1080.0), (0, 0));
        assert_eq!(
            normalize_position(960.0, 540.0, 1920.0, 1080.0),
            (16384, 16384)
        );
        assert_eq!(
            normalize_position(1920.0, 1080.0, 1920.0, 1080.0),
            (32767, 32767)
        );
        assert_eq!(normalize_position(5000.0, -5.0, 1920.0, 1080.0), (32767, 0));
    }

    #[test]
    fn normalize_with_letterbox_offset() {
        // 1280x710 内容区，视频 16:9 -> 上下各 5px 黑边，实际画面 1280x700。
        let (x, y) = normalize_position_in_rect(640.0, 355.0, 0.0, 5.0, 1280.0, 700.0);
        assert_eq!((x, y), (16384, 16384));
        let (x, y) = normalize_position_in_rect(0.0, 5.0, 0.0, 5.0, 1280.0, 700.0);
        assert_eq!((x, y), (0, 0));
        // 黑边内的点被钳制到边缘。
        let (_, y) = normalize_position_in_rect(640.0, 0.0, 0.0, 5.0, 1280.0, 700.0);
        assert_eq!(y, 0);
    }

    #[test]
    fn normalize_with_pillarbox_offset() {
        // 1400x720 内容区，视频 16:9 -> 左右黑边，画面 1280x720，x 偏移 60。
        let (x, y) = normalize_position_in_rect(700.0, 360.0, 60.0, 0.0, 1280.0, 720.0);
        assert_eq!((x, y), (16384, 16384));
        let (x, _) = normalize_position_in_rect(60.0, 360.0, 60.0, 0.0, 1280.0, 720.0);
        assert_eq!(x, 0);
    }

    #[test]
    fn keyboard_state_roundtrip() {
        let mut state = KeyboardState::new();
        let shift = Modifiers {
            shift: true,
            ..Default::default()
        };
        let down = state.key_down(0x04, shift).unwrap();
        assert_eq!(down.payload, vec![0x02, 0x04, 0, 0, 0, 0, 0]);
        let up = state.key_up(0x04, Modifiers::default()).unwrap();
        assert_eq!(up.payload, vec![0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn keyboard_modifier_only_change() {
        let mut state = KeyboardState::new();
        let shift = Modifiers {
            shift: true,
            ..Default::default()
        };
        let change = state.modifiers_changed(shift).unwrap();
        assert_eq!(change.payload, vec![0x02, 0, 0, 0, 0, 0, 0]);
        assert!(state.modifiers_changed(shift).is_none());
        assert_eq!(
            state.release_all().unwrap().payload,
            vec![0, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn keyboard_six_key_limit() {
        let mut state = KeyboardState::new();
        for usage in 0x04..=0x09 {
            assert!(state.key_down(usage, Modifiers::default()).is_some());
        }
        assert!(state.key_down(0x0A, Modifiers::default()).is_none());
        assert_eq!(state.pressed().len(), 6);
    }

    #[test]
    fn mouse_state_and_buttons() {
        let mut mouse = MouseState::new();
        mouse.set_position(16384, 16384);
        assert!(!mouse.is_pressed(MouseButton::Left));
        assert!(mouse.set_button(MouseButton::Left, true));
        assert!(mouse.is_pressed(MouseButton::Left));
        assert!(!mouse.set_button(MouseButton::Left, true));
        assert!(mouse.set_button(MouseButton::Right, true));
        let frame = mouse.frame(1);
        assert_eq!(frame.payload, vec![0x00, 0x40, 0x00, 0x40, 1, 1, 0, 1]);
        let release = mouse.release_buttons();
        assert_eq!(release.payload[4..7], [0, 0, 0]);
        assert!(!mouse.is_pressed(MouseButton::Left));
        assert!(!mouse.is_pressed(MouseButton::Right));
    }
}
