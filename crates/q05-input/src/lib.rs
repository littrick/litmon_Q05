//! Q0.5 输入状态机、HID usage/坐标映射。
//!
//! LF0 目标机实测：
//! - 键槽是 **HID usage**（0x04='a'，0x41=F8，Enter=0x28）；
//! - 鼠标 x/y 是 **0..65535 归一化坐标**（32767,32767 ≈ 屏幕中心）；
//! - 修饰键位图是标准 HID 语义（0x01 LCTRL / 0x02 LSHIFT / 0x04 LALT / 0x08 LWIN）。

use q05_proto::Frame;

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

/// 把窗口内坐标转成 Q0.5 的 0..65535 归一化坐标。
pub fn normalize_position(x: f32, y: f32, width: f32, height: f32) -> (u16, u16) {
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return (0, 0);
    }
    let nx = (x / width).clamp(0.0, 1.0);
    let ny = (y / height).clamp(0.0, 1.0);
    ((nx * 65535.0).round() as u16, (ny * 65535.0).round() as u16)
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
    fn normalize_center_and_edges() {
        assert_eq!(normalize_position(0.0, 0.0, 1920.0, 1080.0), (0, 0));
        assert_eq!(
            normalize_position(960.0, 540.0, 1920.0, 1080.0),
            (32768, 32768)
        );
        assert_eq!(
            normalize_position(1920.0, 1080.0, 1920.0, 1080.0),
            (65535, 65535)
        );
        assert_eq!(normalize_position(5000.0, -5.0, 1920.0, 1080.0), (65535, 0));
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
        mouse.set_position(32768, 32768);
        assert!(mouse.set_button(MouseButton::Left, true));
        assert!(!mouse.set_button(MouseButton::Left, true));
        assert!(mouse.set_button(MouseButton::Right, true));
        let frame = mouse.frame(1);
        assert_eq!(frame.payload, vec![0x00, 0x80, 0x00, 0x80, 1, 1, 0, 1]);
        let release = mouse.release_buttons();
        assert_eq!(release.payload[4..7], [0, 0, 0]);
    }
}
