//! Q0.5 会话编排、生命周期、设备插拔检测。
//!
//! 当前实现：通过 Windows CfgMgr 轮询两个 USB 设备是否存在：
//! - `USB\VID_345F&PID_2131`：视频 + 音频复合设备；
//! - `USB\VID_413D&PID_2107`：输入 HID。

#[cfg(windows)]
mod media;
#[cfg(windows)]
mod presence;

#[cfg(windows)]
pub use media::MediaSupervisor;
#[cfg(windows)]
pub use presence::{DeviceError, DeviceMonitor, DevicePresence, query_presence};

#[cfg(not(windows))]
mod unsupported {
    use async_channel::Sender;
    use std::time::Duration;

    /// 设备在线状态。
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct DevicePresence {
        /// 345F:2131 复合设备（视频/音频）。
        pub composite: bool,
        /// 413D:2107 输入 HID。
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
        /// 当前平台不支持。
        #[error("device presence is not supported on this platform")]
        Unsupported,
    }

    /// 查询一次设备在线状态。
    pub fn query_presence() -> Result<DevicePresence, DeviceError> {
        Err(DeviceError::Unsupported)
    }

    /// 设备插拔轮询器。
    pub struct DeviceMonitor {
        _interval: Duration,
    }

    impl DeviceMonitor {
        /// 启动轮询（非 Windows 平台不可用）。
        pub fn start(_tx: Sender<DevicePresence>) -> std::io::Result<Self> {
            Ok(Self {
                _interval: Duration::from_secs(1),
            })
        }

        /// 停止轮询。
        pub fn stop(&mut self) {}
    }
}

#[cfg(not(windows))]
pub use unsupported::{DeviceError, DeviceMonitor, DevicePresence, query_presence};
