//! litmon-q05 主程序入口。
//!
//! 媒体流由 `q05-session::MediaSupervisor` 管理：设备在线时启动视频/HID/音频，
//! 拔出后停止，重新插入后自动重建。UI 只消费持续的视频/输入/状态通道。
//!
//! 默认使用 Windows GUI 子系统，双击启动不显示控制台；
//! 需要查看日志时使用 `litmon-q05.exe --console`（或 `-c`）。

#![cfg_attr(windows, windows_subsystem = "windows")]

use anyhow::Result;

fn main() -> Result<()> {
    if wants_console(std::env::args().skip(1)) {
        #[cfg(windows)]
        enable_console();
    }

    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let (video_tx, video_rx) = async_channel::bounded(2);
    let (hid_tx, hid_rx) = async_channel::bounded(8);
    let (presence_tx, presence_rx) = async_channel::bounded(8);

    let mut supervisor = q05_session::MediaSupervisor::start(video_tx, hid_tx, presence_tx)?;

    let result = q05_ui::run(video_rx, presence_rx, hid_rx);
    supervisor.stop();
    result
}

/// 是否请求显示控制台。
fn wants_console<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter()
        .any(|arg| matches!(arg.as_ref(), "--console" | "-c"))
}

/// 附加到父进程控制台；若没有则新建一个。
///
/// 已存在的标准句柄（例如 shell 重定向）保持不变。
#[cfg(windows)]
fn enable_console() {
    use windows::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AllocConsole, AttachConsole, STD_ERROR_HANDLE, STD_INPUT_HANDLE,
        STD_OUTPUT_HANDLE,
    };

    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.is_err() {
        // 输出已被 shell 重定向时（句柄有效）不额外弹控制台窗口。
        let redirected =
            console_handle_valid(STD_OUTPUT_HANDLE) && console_handle_valid(STD_ERROR_HANDLE);
        if !redirected {
            unsafe {
                let _ = AllocConsole();
            }
        }
    }

    bind_console_handle(STD_OUTPUT_HANDLE, "CONOUT$");
    bind_console_handle(STD_ERROR_HANDLE, "CONOUT$");
    bind_console_handle(STD_INPUT_HANDLE, "CONIN$");
}

/// 标准句柄是否已可用。
#[cfg(windows)]
fn console_handle_valid(kind: windows::Win32::System::Console::STD_HANDLE) -> bool {
    unsafe {
        windows::Win32::System::Console::GetStdHandle(kind).is_ok_and(|handle| !handle.is_invalid())
    }
}

/// 在标准句柄无效时绑定到控制台设备。
#[cfg(windows)]
fn bind_console_handle(kind: windows::Win32::System::Console::STD_HANDLE, name: &str) {
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING,
    };
    use windows::Win32::System::Console::{GetStdHandle, SetStdHandle};
    use windows::core::PCWSTR;

    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        // 已有句柄说明 stdout/stderr 被重定向，保留重定向。
        if let Ok(handle) = GetStdHandle(kind)
            && !handle.is_invalid()
        {
            return;
        }
        let handle = CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            Default::default(),
            None,
        );
        if let Ok(handle) = handle {
            let _ = SetStdHandle(kind, handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_console_flag() {
        assert!(wants_console(["--console"]));
        assert!(wants_console(["-c"]));
        assert!(wants_console(["foo", "--console"]));
        assert!(!wants_console(std::iter::empty::<&str>()));
        assert!(!wants_console(["--consoles"]));
    }
}
