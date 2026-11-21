//! Windows CfgMgr 设备在线检测。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use async_channel::Sender;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_GETIDLIST_FILTER_PRESENT, CM_Get_Device_ID_List_SizeW, CM_Get_Device_ID_ListW, CR_SUCCESS,
};

/// Q0.5 设备在线状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DevicePresence {
    /// `USB\VID_345F&PID_2131`（视频 + 音频复合设备）。
    pub composite: bool,
    /// `USB\VID_413D&PID_2107`（输入 HID）。
    pub input: bool,
}

impl DevicePresence {
    /// 两个设备都在线。
    pub fn connected(&self) -> bool {
        self.composite && self.input
    }
}

/// 设备检测错误。
#[derive(Debug, Clone, thiserror::Error)]
pub enum DeviceError {
    /// CfgMgr 返回非 CR_SUCCESS。
    #[error("cfg manager error 0x{0:08x}")]
    Config(u32),
    /// 无法读取设备列表。
    #[error("device presence thread: {0}")]
    Thread(String),
}

/// 查询一次当前在线设备。
pub fn query_presence() -> Result<DevicePresence, DeviceError> {
    let mut len = 0u32;
    let ret = unsafe {
        CM_Get_Device_ID_List_SizeW(
            &mut len,
            windows::core::w!("USB"),
            CM_GETIDLIST_FILTER_PRESENT,
        )
    };
    if ret != CR_SUCCESS {
        return Err(DeviceError::Config(ret.0));
    }
    if len == 0 {
        return Ok(DevicePresence::default());
    }

    let mut buffer = vec![0u16; len as usize];
    let ret = unsafe {
        CM_Get_Device_ID_ListW(
            windows::core::w!("USB"),
            &mut buffer,
            CM_GETIDLIST_FILTER_PRESENT,
        )
    };
    if ret != CR_SUCCESS {
        return Err(DeviceError::Config(ret.0));
    }
    Ok(parse_device_ids(&buffer))
}

fn parse_device_ids(buffer: &[u16]) -> DevicePresence {
    let mut presence = DevicePresence::default();
    for part in buffer.split(|&c| c == 0) {
        if part.is_empty() {
            continue;
        }
        let id = String::from_utf16_lossy(part).to_ascii_uppercase();
        if id.starts_with("USB\\VID_345F&PID_2131") {
            presence.composite = true;
        } else if id.starts_with("USB\\VID_413D&PID_2107") {
            presence.input = true;
        }
    }
    presence
}

/// 轮询设备插拔的监视器。
///
/// 启动后立即查询一次，之后每 500ms 查询；状态变化时通过 channel 发送。
pub struct DeviceMonitor {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl DeviceMonitor {
    /// 启动监视器。
    pub fn start(tx: Sender<DevicePresence>) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let thread = thread::Builder::new()
            .name("q05-presence".into())
            .spawn(move || run_monitor(tx, stop_thread))?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }

    /// 停止并等待线程。
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for DeviceMonitor {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_monitor(tx: Sender<DevicePresence>, stop: Arc<AtomicBool>) {
    let mut last: Option<DevicePresence> = None;
    while !stop.load(Ordering::Relaxed) {
        match query_presence() {
            Ok(presence) => {
                if last != Some(presence) {
                    tracing::info!(?presence, "device presence changed");
                    let _ = tx.try_send(presence);
                    last = Some(presence);
                }
            }
            Err(err) => {
                tracing::warn!(error = %err, "query device presence failed");
            }
        }
        thread::sleep(Duration::from_millis(500));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn multi_sz(ids: &[&str]) -> Vec<u16> {
        let mut buffer = Vec::new();
        for id in ids {
            buffer.extend(id.encode_utf16());
            buffer.push(0);
        }
        buffer.push(0);
        buffer
    }

    #[test]
    fn parses_both_devices() {
        let buffer = multi_sz(&[
            "USB\\VID_345F&PID_2131\\14402703",
            "USB\\VID_413D&PID_2107\\20240415",
            "USB\\VID_1A40&PID_0101\\5&2A99FA97&0&6",
        ]);
        let presence = parse_device_ids(&buffer);
        assert!(presence.composite);
        assert!(presence.input);
        assert!(presence.connected());
    }

    #[test]
    fn detects_missing_input() {
        let buffer = multi_sz(&["USB\\VID_345F&PID_2131\\14402703"]);
        let presence = parse_device_ids(&buffer);
        assert!(presence.composite);
        assert!(!presence.input);
        assert!(!presence.connected());
    }

    #[test]
    fn unrelated_devices_not_matched() {
        let buffer = multi_sz(&["USB\\VID_1A40&PID_0101\\5&2A99FA97&0&6"]);
        assert_eq!(parse_device_ids(&buffer), DevicePresence::default());
    }
}
