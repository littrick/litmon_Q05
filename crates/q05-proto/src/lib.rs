//! Oray Q0.5 输入 HID 私有协议编解码（`413D:2107`）。
//!
//! 本文件移植自上一轮逆向工程 `reverse_Q05/q05-tool/src/lib.rs`，
//! 真机样本与测试保持不变；后续在本 crate 内继续扩展命令与健壮性测试。
//!
//! Wire format (65-byte HID reports, report id 0):
//!
//! ```text
//!   AA | len(u16 LE) | field(u16 LE) | checksum(u16 LE) | cmd | payload... | BB
//!
//!   len       = payload_len + 6
//!   checksum  = (0xBB + cmd + sum(payload)) & 0xffff
//!   escaping  : 0xAA -> CC 02,  0xCC -> CC 01   (applies to every byte
//!               between AA and BB, including len/field/checksum)
//! ```
//!
//! Only `AA` and `CC` are escaped; `BB` terminates the frame.  A HID report may
//! contain stale bytes after `BB` (the firmware does not clear its report
//! buffer), so a decoder must stop at `BB` instead of using the 65-byte length.

#![forbid(unsafe_code)]

use std::fmt::Write as _;

pub const VID: u16 = 0x413D;
pub const PID: u16 = 0x2107;
/// Display / audio / camera USB device (vendor HID control interface MI_04).
pub const DISPLAY_VID: u16 = 0x345F;
pub const DISPLAY_PID: u16 = 0x2131;

/// Both InputReportByteLength and OutputReportByteLength are 65.
pub const REPORT_LEN: usize = 65;
/// Default value of the `field` word in every observed frame.
pub const DEFAULT_FIELD: u16 = 0x0100;

pub const START: u8 = 0xAA;
pub const END: u8 = 0xBB;
pub const ESC: u8 = 0xCC;

pub mod cmd {
    pub const HANDSHAKE: u8 = 0x01;
    pub const MOUSE_ABS: u8 = 0x02;
    pub const KEYBOARD: u8 = 0x03;
    pub const MOUSE_REL: u8 = 0x04;
    pub const SET_LED: u8 = 0x05;
    pub const GET_LED: u8 = 0x06;
    pub const FIRMWARE: u8 = 0x07;
    pub const GET_DEVICE_INFO: u8 = 0x09;
    pub const SET_DEVICE_INFO: u8 = 0x0A;
    pub const MOUSE_STATE: u8 = 0x0C;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub field: u16,
    pub cmd: u8,
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    NoStart,
    NoEnd,
    TooShort(usize),
    BadLength { declared: u16, actual: usize },
    BadChecksum { declared: u16, computed: u16 },
    BadEscape,
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::NoStart => write!(f, "no 0xAA start byte"),
            FrameError::NoEnd => write!(f, "no unescaped 0xBB end byte"),
            FrameError::TooShort(n) => write!(f, "frame body too short ({n} bytes)"),
            FrameError::BadLength { declared, actual } => {
                write!(f, "length field {declared} != actual {actual}")
            }
            FrameError::BadChecksum { declared, computed } => {
                write!(f, "checksum 0x{declared:04X} != computed 0x{computed:04X}")
            }
            FrameError::BadEscape => write!(f, "invalid CC escape sequence"),
        }
    }
}

impl std::error::Error for FrameError {}

impl Frame {
    pub fn new(cmd: u8, payload: impl Into<Vec<u8>>) -> Self {
        Frame {
            field: DEFAULT_FIELD,
            cmd,
            payload: payload.into(),
        }
    }

    /// 握手 / 版本查询（cmd `0x01`）。
    pub fn handshake() -> Self {
        Self::new(cmd::HANDSHAKE, Vec::new())
    }

    /// 读取设备信息（cmd `0x09`）。
    pub fn get_device_info() -> Self {
        Self::new(cmd::GET_DEVICE_INFO, Vec::new())
    }

    /// 鼠标绝对事件（cmd `0x02`）；坐标是 0..65535 归一化值。
    pub fn mouse_abs(x: u16, y: u16, left: bool, right: bool, middle: bool, wheel: i8) -> Self {
        let mut payload = Vec::with_capacity(8);
        payload.extend_from_slice(&x.to_le_bytes());
        payload.extend_from_slice(&y.to_le_bytes());
        payload.push(u8::from(left));
        payload.push(u8::from(right));
        payload.push(u8::from(middle));
        payload.push(wheel as u8);
        Self::new(cmd::MOUSE_ABS, payload)
    }

    /// 鼠标相对事件（cmd `0x04`）。
    pub fn mouse_rel(dx: i16, dy: i16, left: bool, right: bool, middle: bool, wheel: i8) -> Self {
        let mut payload = Vec::with_capacity(8);
        payload.extend_from_slice(&dx.to_le_bytes());
        payload.extend_from_slice(&dy.to_le_bytes());
        payload.push(u8::from(left));
        payload.push(u8::from(right));
        payload.push(u8::from(middle));
        payload.push(wheel as u8);
        Self::new(cmd::MOUSE_REL, payload)
    }

    /// 键盘报告（cmd `0x03`）；keys 是 HID usage，最多 6 个。
    pub fn keyboard(modifiers: u8, keys: [u8; 6]) -> Self {
        let mut payload = Vec::with_capacity(7);
        payload.push(modifiers);
        payload.extend_from_slice(&keys);
        Self::new(cmd::KEYBOARD, payload)
    }

    /// 释放所有键（全 0 键盘报告）。
    pub fn keyboard_release() -> Self {
        Self::keyboard(0, [0; 6])
    }

    /// 设置 LED / 状态寄存器（cmd `0x05`）。
    pub fn set_led(value: u8) -> Self {
        Self::new(cmd::SET_LED, vec![value])
    }

    /// 读取 LED / 状态寄存器（cmd `0x06`）。
    pub fn get_led() -> Self {
        Self::new(cmd::GET_LED, vec![0])
    }

    /// `(0xBB + cmd + sum(payload)) & 0xffff`
    pub fn checksum(&self) -> u16 {
        let mut sum: u32 = 0xBB + self.cmd as u32;
        for b in &self.payload {
            sum += *b as u32;
        }
        (sum & 0xffff) as u16
    }

    /// Encode header + payload, escaped, framed by `AA` ... `BB`.
    ///
    /// This is the 9..=64-byte body that follows the HID report-id byte.
    pub fn encode(&self) -> Vec<u8> {
        let len = (self.payload.len() + 6) as u16;
        let chk = self.checksum();
        let mut body = Vec::with_capacity(self.payload.len() + 7);
        body.extend_from_slice(&len.to_le_bytes());
        body.extend_from_slice(&self.field.to_le_bytes());
        body.extend_from_slice(&chk.to_le_bytes());
        body.push(self.cmd);
        body.extend_from_slice(&self.payload);

        let mut out = Vec::with_capacity(body.len() + 4);
        out.push(START);
        for b in body {
            match b {
                START => out.extend_from_slice(&[ESC, 0x02]),
                ESC => out.extend_from_slice(&[ESC, 0x01]),
                _ => out.push(b),
            }
        }
        out.push(END);
        out
    }

    /// Decode the first frame found in `buf`.
    ///
    /// Stops at the first unescaped `BB`; anything after it is ignored.
    ///
    /// 注意：该函数按“第一个 `0xBB` 即结束”解析。目标机已确认 payload 可以包含
    /// `0xBB`（例如坐标 `0x00BB`），这类帧请使用 [`Frame::decode_lenient`]。
    pub fn decode(buf: &[u8]) -> Result<Frame, FrameError> {
        let start = buf
            .iter()
            .position(|&b| b == START)
            .ok_or(FrameError::NoStart)?;
        decode_from(&buf[start + 1..])
    }

    /// 长度感知解码：先反转义出 `len`，再按 `len + 1` 读取 body，最后校验 `0xBB`。
    ///
    /// 即使 payload 含 `0xBB` 也能正确解析；返回 `(frame, consumed)`，
    /// `consumed` 从 `buf` 起始位置算起（含 `AA` 与 `BB`）。
    pub fn decode_lenient(buf: &[u8]) -> Result<(Frame, usize), FrameError> {
        let start = buf
            .iter()
            .position(|&b| b == START)
            .ok_or(FrameError::NoStart)?;
        let (frame, body_and_end) = decode_lenient_from(&buf[start + 1..])?;
        Ok((frame, start + 1 + body_and_end))
    }

    /// Decode without searching for `AA`; `buf` starts right after the start byte.
    ///
    /// 与 [`Frame::decode`] 一样按第一个 `0xBB` 结束；payload 可能含 `0xBB` 时请用
    /// [`Frame::decode_lenient`]。
    pub fn decode_after_start(buf: &[u8]) -> Result<Frame, FrameError> {
        decode_from(buf)
    }
}

fn unescape_next(buf: &[u8], i: usize) -> Result<(u8, usize), FrameError> {
    let b = *buf.get(i).ok_or(FrameError::NoEnd)?;
    if b == ESC {
        match buf.get(i + 1) {
            Some(0x02) => Ok((START, i + 2)),
            Some(0x01) => Ok((ESC, i + 2)),
            _ => Err(FrameError::BadEscape),
        }
    } else {
        Ok((b, i + 1))
    }
}

/// 从 `AA` 之后开始解析；返回 `(frame, body+BB 的字节数)`。
fn decode_lenient_from(buf: &[u8]) -> Result<(Frame, usize), FrameError> {
    let mut raw = Vec::with_capacity(64);
    let mut i = 0;

    // 先读 len(u16 LE) 两个字节，才能知道 body 长度。
    while raw.len() < 2 {
        let (b, next) = unescape_next(buf, i)?;
        raw.push(b);
        i = next;
    }
    let len = u16::from_le_bytes([raw[0], raw[1]]) as usize;
    // len 统计 field(2)+checksum(2)+cmd(1)+payload+BB(1)；
    // raw body（不含 AA/BB）长度为 len + 1。
    let body_len = len + 1;
    if body_len < 7 {
        return Err(FrameError::TooShort(body_len));
    }
    while raw.len() < body_len {
        let (b, next) = unescape_next(buf, i)?;
        raw.push(b);
        i = next;
    }
    if buf.get(i) != Some(&END) {
        return Err(FrameError::NoEnd);
    }
    i += 1;

    let field = u16::from_le_bytes([raw[2], raw[3]]);
    let declared_chk = u16::from_le_bytes([raw[4], raw[5]]);
    let cmd = raw[6];
    let payload = raw[7..].to_vec();
    let frame = Frame {
        field,
        cmd,
        payload,
    };
    let computed = frame.checksum();
    if computed != declared_chk {
        return Err(FrameError::BadChecksum {
            declared: declared_chk,
            computed,
        });
    }
    Ok((frame, i))
}

fn decode_from(buf: &[u8]) -> Result<Frame, FrameError> {
    let mut raw = Vec::with_capacity(buf.len());
    let mut i = 0;
    let mut ended = false;
    while i < buf.len() {
        let b = buf[i];
        if b == ESC {
            match buf.get(i + 1) {
                Some(0x02) => raw.push(START),
                Some(0x01) => raw.push(ESC),
                _ => return Err(FrameError::BadEscape),
            }
            i += 2;
        } else if b == END {
            ended = true;
            break;
        } else {
            raw.push(b);
            i += 1;
        }
    }
    if !ended {
        return Err(FrameError::NoEnd);
    }
    if raw.len() < 7 {
        return Err(FrameError::TooShort(raw.len()));
    }
    let len = u16::from_le_bytes([raw[0], raw[1]]);
    let field = u16::from_le_bytes([raw[2], raw[3]]);
    let declared_chk = u16::from_le_bytes([raw[4], raw[5]]);
    let cmd = raw[6];
    let payload = raw[7..].to_vec();
    // len counts field+checksum+cmd+payload+BB == raw.len() + 1
    if raw.len() != len as usize + 1 {
        return Err(FrameError::BadLength {
            declared: len,
            actual: raw.len() - 1,
        });
    }
    let frame = Frame {
        field,
        cmd,
        payload,
    };
    let computed = frame.checksum();
    if computed != declared_chk {
        return Err(FrameError::BadChecksum {
            declared: declared_chk,
            computed,
        });
    }
    Ok(frame)
}

/// Build the full 65-byte HID output report (report id 0 + frame + zero pad).
pub fn output_report(frame: &Frame) -> [u8; REPORT_LEN] {
    let mut buf = [0u8; REPORT_LEN];
    let enc = frame.encode();
    let n = enc.len().min(REPORT_LEN - 1);
    buf[1..1 + n].copy_from_slice(&enc[..n]);
    buf
}

pub fn command_name(cmd: u8) -> &'static str {
    match cmd {
        cmd::HANDSHAKE => "handshake",
        cmd::MOUSE_ABS => "mouse-abs",
        cmd::KEYBOARD => "keyboard",
        cmd::MOUSE_REL => "mouse-rel",
        cmd::SET_LED => "set-led",
        cmd::GET_LED => "get-led",
        cmd::FIRMWARE => "firmware",
        cmd::GET_DEVICE_INFO => "get-device-info",
        cmd::SET_DEVICE_INFO => "set-device-info",
        cmd::MOUSE_STATE => "mouse-state",
        _ => "unknown",
    }
}

pub fn modifier_names(m: u8) -> String {
    const BIT: [(u8, &str); 8] = [
        (0x01, "LCTRL"),
        (0x02, "LSHIFT"),
        (0x04, "LALT"),
        (0x08, "LWIN"),
        (0x10, "RCTRL"),
        (0x20, "RSHIFT"),
        (0x40, "RALT"),
        (0x80, "RWIN"),
    ];
    let mut v: Vec<&str> = BIT
        .iter()
        .filter(|(b, _)| m & b != 0)
        .map(|(_, n)| *n)
        .collect();
    if v.is_empty() {
        v.push("-");
    }
    v.join("|")
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 3);
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 {
            s.push(' ');
        }
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn cstr(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn u16le(p: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([p[o], p[o + 1]])
}

fn buttons(left: bool, right: bool, middle: bool, wheel: i8) -> String {
    let mut s = String::new();
    if left {
        s.push_str("L ");
    }
    if right {
        s.push_str("R ");
    }
    if middle {
        s.push_str("M ");
    }
    if s.is_empty() {
        s.push_str("none");
    } else {
        s.pop();
    }
    if wheel != 0 {
        let _ = write!(s, " wheel={wheel}");
    }
    s
}

fn mouse_abs(p: &[u8]) -> String {
    if p.len() < 8 {
        return format!("short mouse payload ({} bytes)", p.len());
    }
    format!(
        "x={} y={} buttons=[{}]",
        u16le(p, 0),
        u16le(p, 2),
        buttons(p[4] != 0, p[5] != 0, p[6] != 0, p[7] as i8)
    )
}

fn mouse_rel(p: &[u8]) -> String {
    if p.len() < 8 {
        return format!("short relative-mouse payload ({} bytes)", p.len());
    }
    format!(
        "dx={} dy={} buttons=[{}]",
        i16::from_le_bytes([p[0], p[1]]),
        i16::from_le_bytes([p[2], p[3]]),
        buttons(p[4] != 0, p[5] != 0, p[6] != 0, p[7] as i8)
    )
}

fn keyboard(p: &[u8]) -> String {
    if p.len() < 7 {
        return format!("short keyboard payload ({} bytes)", p.len());
    }
    let keys: Vec<String> = p[1..7]
        .iter()
        .filter(|&&k| k != 0)
        .map(|k| format!("{k:02x}"))
        .collect();
    format!(
        "modifiers=[{}] keys=[{}]",
        modifier_names(p[0]),
        if keys.is_empty() {
            "-".into()
        } else {
            keys.join(" ")
        }
    )
}

fn handshake(p: &[u8]) -> String {
    if p.len() < 20 {
        return format!("short handshake payload ({} bytes)", p.len());
    }
    format!(
        "fw_version={:?} chip={:?}",
        cstr(&p[0..10]),
        cstr(&p[10..20])
    )
}

/// `0x09` response layout: 20-byte name, 12-byte serial, 1-byte status.
fn device_info(p: &[u8]) -> String {
    if p.len() < 20 {
        return format!("short device-info payload ({} bytes)", p.len());
    }
    let name = cstr(&p[0..20]);
    let serial = cstr(&p[20..p.len().min(32)]);
    match p.get(32) {
        Some(st) => format!("name={name:?} serial={serial:?} status=0x{st:02x}"),
        None => format!("name={name:?} serial={serial:?}"),
    }
}

fn ack(payload: &[u8]) -> Option<String> {
    match payload {
        [status] => Some(format!("ack status=0x{status:02x}")),
        _ => None,
    }
}

/// Human readable description of a decoded frame.
pub fn describe(frame: &Frame) -> String {
    let body = match frame.cmd {
        cmd::HANDSHAKE => handshake(&frame.payload),
        cmd::MOUSE_ABS => ack(&frame.payload).unwrap_or_else(|| mouse_abs(&frame.payload)),
        cmd::KEYBOARD => ack(&frame.payload).unwrap_or_else(|| keyboard(&frame.payload)),
        cmd::MOUSE_REL => ack(&frame.payload).unwrap_or_else(|| mouse_rel(&frame.payload)),
        cmd::SET_LED => match frame.payload.first() {
            Some(v) => format!("value={v}"),
            None => "missing value".into(),
        },
        cmd::GET_LED => match frame.payload.first() {
            Some(v) => format!("value={v}"),
            None => "missing value".into(),
        },
        cmd::FIRMWARE => format!(
            "{} bytes: {}",
            frame.payload.len(),
            hex(&frame.payload[..frame.payload.len().min(16)])
        ),
        cmd::GET_DEVICE_INFO => device_info(&frame.payload),
        cmd::SET_DEVICE_INFO => format!("data={:?}", cstr(&frame.payload)),
        cmd::MOUSE_STATE => ack(&frame.payload).unwrap_or_else(|| {
            if frame.payload.len() < 7 {
                format!("short mouse-state payload ({} bytes)", frame.payload.len())
            } else {
                format!(
                    "flag=0x{:02x} button={} x={} y={}",
                    frame.payload[0],
                    frame.payload[1],
                    u16le(&frame.payload, 3),
                    u16le(&frame.payload, 5)
                )
            }
        }),
        _ => format!("{} bytes: {}", frame.payload.len(), hex(&frame.payload)),
    };
    format!(
        "cmd=0x{:02x} ({}) field=0x{:04x} len={} chk=0x{:04x} :: {}",
        frame.cmd,
        command_name(frame.cmd),
        frame.field,
        frame.payload.len() + 6,
        frame.checksum(),
        body
    )
}

/// Decode a hex string ("aa 07 00 ..." or "aa0700...") and describe each frame.
pub fn describe_hex(input: &str) -> Result<Vec<String>, String> {
    let clean: String = input.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if !clean.len().is_multiple_of(2) {
        return Err("hex string has odd length".into());
    }
    let mut bytes = Vec::with_capacity(clean.len() / 2);
    for i in (0..clean.len()).step_by(2) {
        bytes.push(u8::from_str_radix(&clean[i..i + 2], 16).map_err(|e| e.to_string())?);
    }
    let mut out = Vec::new();
    let mut rest = &bytes[..];
    while let Some(start) = rest.iter().position(|&b| b == START) {
        match Frame::decode_lenient(&rest[start..]) {
            Ok((frame, consumed)) => {
                out.push(describe(&frame));
                rest = &rest[start + consumed..];
            }
            Err(_) => {
                // 跳过这个坏起点，继续找下一个 AA。
                rest = &rest[start + 1..];
            }
        }
    }
    if out.is_empty() {
        Err("no decodable frame".into())
    } else {
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_encoding() {
        let f = Frame::new(cmd::HANDSHAKE, vec![]);
        assert_eq!(
            f.encode(),
            vec![0xAA, 0x06, 0x00, 0x00, 0x01, 0xBC, 0x00, 0x01, 0xBB]
        );
    }

    #[test]
    fn escaping_roundtrip() {
        let f = Frame::new(cmd::MOUSE_ABS, vec![0xAA, 0x00, 0xCC, 0x00, 0, 0, 0, 0]);
        let enc = f.encode();
        assert_eq!(
            enc,
            vec![
                0xAA, 0x0e, 0x00, 0x00, 0x01, 0x33, 0x02, 0x02, 0xCC, 0x02, 0x00, 0xCC, 0x01, 0x00,
                0x00, 0x00, 0x00, 0x00, 0xBB
            ]
        );
        assert_eq!(Frame::decode(&enc).unwrap(), f);
    }

    #[test]
    fn decode_handshake_response() {
        let raw = hex_bytes(
            "00 aa 2e 00 00 01 79 03 01 31 2e 30 2e 30 00 00 00 00 00 63 68 35 38 32 66 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 bb 00 00",
        );
        let f = Frame::decode(&raw).unwrap();
        assert_eq!(f.cmd, 1);
        assert!(describe(&f).contains("fw_version=\"1.0.0\""));
        assert!(describe(&f).contains("chip=\"ch582f\""));
    }

    #[test]
    fn decode_device_info_response() {
        let raw = hex_bytes(
            "00 aa 27 00 00 01 cd 05 09 4f 72 61 79 20 51 30 2e 35 00 00 00 00 00 00 00 00 00 00 00 34 33 36 30 37 35 31 33 31 32 34 36 00 bb",
        );
        let f = Frame::decode(&raw).unwrap();
        assert!(describe(&f).contains("name=\"Oray Q0.5\""));
        assert!(describe(&f).contains("serial=\"436075131246\""));
    }

    #[test]
    fn mouse_payload_decoding() {
        let raw = hex_bytes("aa 0e 00 00 01 e9 01 02 64 00 c8 00 00 00 00 00 bb");
        let f = Frame::decode(&raw).unwrap();
        assert!(describe(&f).contains("x=100 y=200"));
    }

    #[test]
    fn lenient_handshake_roundtrip() {
        let f = Frame::new(cmd::HANDSHAKE, vec![]);
        let enc = f.encode();
        let (decoded, consumed) = Frame::decode_lenient(&enc).unwrap();
        assert_eq!(decoded, f);
        assert_eq!(consumed, enc.len());
    }

    #[test]
    fn lenient_payload_containing_end_byte() {
        // 目标机已确认 payload 可以包含 0xBB（例如坐标 0x00BB）。
        let f = Frame::new(cmd::MOUSE_ABS, vec![0xBB, 0x00, 0xBB, 0x00, 0, 0, 0, 0]);
        let enc = f.encode();
        let (decoded, consumed) = Frame::decode_lenient(&enc).unwrap();
        assert_eq!(decoded, f);
        assert_eq!(consumed, enc.len());
        assert!(Frame::decode(&enc).is_err(), "legacy decoder should fail");
    }

    #[test]
    fn lenient_ignores_trailing_stale_bytes() {
        let f = Frame::new(cmd::MOUSE_ABS, vec![100, 0, 200, 0, 0, 0, 0, 0]);
        let mut buf = f.encode();
        let frame_len = buf.len();
        buf.extend_from_slice(&[0xBB, 0x00, 0x00, 0xAA, 0x55]);
        let (decoded, consumed) = Frame::decode_lenient(&buf).unwrap();
        assert_eq!(decoded, f);
        assert_eq!(consumed, frame_len);
    }

    #[test]
    fn lenient_decodes_multiple_frames() {
        let a = Frame::new(cmd::HANDSHAKE, vec![]);
        let b = Frame::new(cmd::MOUSE_ABS, vec![1, 0, 2, 0, 0, 0, 0, 0]);
        let mut buf = a.encode();
        buf.extend_from_slice(&b.encode());
        let (first, consumed) = Frame::decode_lenient(&buf).unwrap();
        assert_eq!(first, a);
        let (second, consumed2) = Frame::decode_lenient(&buf[consumed..]).unwrap();
        assert_eq!(second, b);
        assert_eq!(consumed + consumed2, buf.len());
    }

    #[test]
    fn lenient_rejects_truncated_frame() {
        let f = Frame::new(cmd::MOUSE_ABS, vec![1, 0, 2, 0, 0, 0, 0, 0]);
        let enc = f.encode();
        assert!(Frame::decode_lenient(&enc[..enc.len() - 1]).is_err());
    }

    #[test]
    fn mouse_abs_constructor() {
        let f = Frame::mouse_abs(100, 200, true, false, false, -1);
        assert_eq!(f.cmd, cmd::MOUSE_ABS);
        assert_eq!(f.payload, vec![100, 0, 200, 0, 1, 0, 0, 0xff]);
    }

    #[test]
    fn mouse_rel_constructor() {
        let f = Frame::mouse_rel(-5, 3, false, true, false, 1);
        assert_eq!(f.cmd, cmd::MOUSE_REL);
        assert_eq!(f.payload, vec![0xfb, 0xff, 3, 0, 0, 1, 0, 1]);
    }

    #[test]
    fn keyboard_constructors() {
        let f = Frame::keyboard(0x02, [0x04, 0, 0, 0, 0, 0]);
        assert_eq!(f.cmd, cmd::KEYBOARD);
        assert_eq!(f.payload, vec![0x02, 0x04, 0, 0, 0, 0, 0]);
        assert_eq!(Frame::keyboard_release().payload, vec![0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn led_constructors() {
        assert_eq!(Frame::set_led(1).payload, vec![1]);
        assert_eq!(Frame::get_led().payload, vec![0]);
        assert_eq!(Frame::handshake().cmd, cmd::HANDSHAKE);
        assert_eq!(Frame::get_device_info().cmd, cmd::GET_DEVICE_INFO);
    }

    fn hex_bytes(s: &str) -> Vec<u8> {
        let c: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        (0..c.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&c[i..i + 2], 16).unwrap())
            .collect()
    }
}
