//! Q0.5 输入 HID 传输（413D:2107）。
//!
//! 设计：
//! - 一个专用 I/O 线程持有 `hidapi::HidDevice`；
//! - UI/输入层通过有界 channel 提交 [`Frame`]；
//! - I/O 线程写入 65 字节报告，并用短超时读取 ack / 心跳；
//! - 队列满时 [`HidHandle::send`] 返回 [`HidError::Busy`]，由输入层丢移动帧。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use async_channel::{Sender, TrySendError};
use hidapi::{HidApi, HidDevice};
use q05_proto::{Frame, output_report};

pub use q05_proto::Frame as InputFrame;

/// Q0.5 输入 HID 的 VID。
pub const VID: u16 = 0x413D;
/// Q0.5 输入 HID 的 PID。
pub const PID: u16 = 0x2107;
/// 厂商 HID usage page。
pub const USAGE_PAGE: u16 = 0xFFA0;
/// 厂商 HID usage。
pub const USAGE: u16 = 0x0001;

/// HID 传输错误。
#[derive(Debug, thiserror::Error)]
pub enum HidError {
    /// hidapi 错误。
    #[error("hidapi: {0}")]
    Hid(#[from] hidapi::HidError),
    /// 未找到 Q0.5 输入设备。
    #[error("Oray Q0.5 input HID (413D:2107) not found")]
    NotFound,
    /// 写入队列已满。
    #[error("input queue full")]
    Busy,
    /// I/O 线程启动/运行错误。
    #[error("hid thread: {0}")]
    Thread(String),
    /// 线程创建等 I/O 错误。
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Default)]
struct Stats {
    sent: AtomicU64,
    acks: AtomicU64,
    heartbeats: AtomicU64,
    read_errors: AtomicU64,
    write_errors: AtomicU64,
}

/// HID 统计快照。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HidStats {
    /// 成功写入的帧数。
    pub sent: u64,
    /// 收到的普通应答数。
    pub acks: u64,
    /// 收到的心跳数（cmd 0x05/0x09）。
    pub heartbeats: u64,
    /// 读取错误数。
    pub read_errors: u64,
    /// 写入错误数。
    pub write_errors: u64,
}

/// 已打开的 Q0.5 输入 HID。
///
/// 持有期间 I/O 线程持续运行；drop 时停止并 join。
pub struct HidHandle {
    tx: Sender<Frame>,
    stop: Arc<AtomicBool>,
    stats: Arc<Stats>,
    thread: Option<JoinHandle<()>>,
}

impl HidHandle {
    /// 枚举并打开 `413D:2107`，启动 I/O 线程。
    pub fn open() -> Result<Self, HidError> {
        let api = HidApi::new()?;
        let mut device: Option<HidDevice> = None;
        for info in api.device_list() {
            if info.vendor_id() != VID || info.product_id() != PID {
                continue;
            }
            // 345F:2131 MI_04 也是 FFA0/0001，必须按 VID 排除；
            // 这里再校验 usage 以防万一。
            if info.usage_page() != USAGE_PAGE || info.usage() != USAGE {
                tracing::debug!(
                    usage_page = info.usage_page(),
                    usage = info.usage(),
                    "skip non-matching 413D interface"
                );
                continue;
            }
            match api.open_path(info.path()) {
                Ok(dev) => {
                    tracing::info!(
                        path = ?info.path(),
                        serial = info.serial_number(),
                        "opened Q0.5 input HID"
                    );
                    device = Some(dev);
                    break;
                }
                Err(err) => {
                    tracing::warn!(error = %err, "open Q0.5 input HID failed, trying next");
                }
            }
        }
        let device = device.ok_or(HidError::NotFound)?;

        let (tx, rx) = async_channel::bounded(64);
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(Stats::default());
        let stop_thread = stop.clone();
        let stats_thread = stats.clone();
        let thread = thread::Builder::new()
            .name("q05-hid".into())
            .spawn(move || io_loop(device, rx, stop_thread, stats_thread))?;

        Ok(Self {
            tx,
            stop,
            stats,
            thread: Some(thread),
        })
    }

    /// 提交一帧；队列满时返回 [`HidError::Busy`]。
    pub fn send(&self, frame: Frame) -> Result<(), HidError> {
        match self.tx.try_send(frame) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(HidError::Busy),
            Err(TrySendError::Closed(_)) => Err(HidError::Thread("hid writer closed".into())),
        }
    }

    /// 当前统计。
    pub fn stats(&self) -> HidStats {
        HidStats {
            sent: self.stats.sent.load(Ordering::Relaxed),
            acks: self.stats.acks.load(Ordering::Relaxed),
            heartbeats: self.stats.heartbeats.load(Ordering::Relaxed),
            read_errors: self.stats.read_errors.load(Ordering::Relaxed),
            write_errors: self.stats.write_errors.load(Ordering::Relaxed),
        }
    }

    /// 停止 I/O 线程并 join。
    pub fn close(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for HidHandle {
    fn drop(&mut self) {
        self.close();
    }
}

fn io_loop(
    device: HidDevice,
    rx: async_channel::Receiver<Frame>,
    stop: Arc<AtomicBool>,
    stats: Arc<Stats>,
) {
    // 启动时发一次握手，获取固件版本/芯片；应答会在读循环里解析并记录。
    if let Err(err) = device.write(&output_report(&Frame::handshake())) {
        stats.write_errors.fetch_add(1, Ordering::Relaxed);
        tracing::warn!(error = %err, "handshake write failed");
    }

    let mut buf = [0u8; 65];
    let mut last_write = Instant::now();
    let mut last_mouse: Option<(u16, u16)> = None;
    while !stop.load(Ordering::Relaxed) {
        while let Ok(frame) = rx.try_recv() {
            if frame.cmd == q05_proto::cmd::MOUSE_ABS && frame.payload.len() >= 4 {
                last_mouse = Some((
                    u16::from_le_bytes([frame.payload[0], frame.payload[1]]),
                    u16::from_le_bytes([frame.payload[2], frame.payload[3]]),
                ));
            }
            match device.write(&output_report(&frame)) {
                Ok(_) => {
                    stats.sent.fetch_add(1, Ordering::Relaxed);
                }
                Err(err) => {
                    stats.write_errors.fetch_add(1, Ordering::Relaxed);
                    tracing::warn!(error = %err, cmd = frame.cmd as u32, "hid write failed");
                }
            }
            // 端点 bInterval=10ms；写入之间留出间隔，避免打爆设备队列。
            let elapsed = last_write.elapsed();
            if elapsed < Duration::from_millis(5) {
                thread::sleep(Duration::from_millis(5) - elapsed);
            }
            last_write = Instant::now();
        }

        match device.read_timeout(&mut buf, 5) {
            Ok(0) => {}
            Ok(n) => {
                if let Ok((frame, _)) = Frame::decode_lenient(&buf[..n]) {
                    match frame.cmd {
                        0x05 | 0x09 => {
                            stats.heartbeats.fetch_add(1, Ordering::Relaxed);
                        }
                        _ => {
                            stats.acks.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    tracing::trace!(cmd = frame.cmd as u32, "hid rx");
                }
            }
            Err(err) => {
                stats.read_errors.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(error = %err, "hid read failed");
            }
        }
    }

    // 退出前主动释放键鼠，避免窗口关闭/应用退出后目标端残留按键或拖拽。
    let _ = device.write(&output_report(&Frame::keyboard_release()));
    let (x, y) = last_mouse.unwrap_or((0, 0));
    let _ = device.write(&output_report(&Frame::mouse_abs(
        x, y, false, false, false, 0,
    )));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_protocol() {
        assert_eq!(VID, 0x413D);
        assert_eq!(PID, 0x2107);
        assert_eq!(USAGE_PAGE, 0xFFA0);
        assert_eq!(USAGE, 0x0001);
    }

    #[test]
    fn stats_default_is_zero() {
        let stats = HidStats::default();
        assert_eq!(stats.sent, 0);
        assert_eq!(stats.heartbeats, 0);
    }
}
