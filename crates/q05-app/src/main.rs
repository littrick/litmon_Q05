//! litmon-q05 主程序入口。
//!
//! 媒体流由 `q05-session::MediaSupervisor` 管理：设备在线时启动视频/HID/音频，
//! 拔出后停止，重新插入后自动重建。UI 只消费持续的视频/输入/状态通道。
//!
//! 默认使用 Windows GUI 子系统，双击启动不显示控制台；
//! 从终端启动时自动附加到父控制台（可看到日志、可用 Ctrl+C 结束），
//! 需要强制显示控制台时使用 `litmon-q05.exe --console`（或 `-c`）。

#![cfg_attr(windows, windows_subsystem = "windows")]

use anyhow::Result;

fn main() -> Result<()> {
    #[cfg(windows)]
    let console = setup_console(wants_console(std::env::args().skip(1)));
    #[cfg(not(windows))]
    let console = false;
    #[cfg(windows)]
    if console {
        install_ctrl_handler();
    }

    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();
    tracing::info!(console, "litmon-q05 starting");

    let (video_tx, video_rx) = async_channel::bounded(2);
    let (hid_tx, hid_rx) = async_channel::bounded(8);
    let (presence_tx, presence_rx) = async_channel::bounded(8);
    let (audio_tx, audio_rx) = async_channel::bounded(8);
    // UI 与音频引擎共享的本地静音开关。
    let audio_mute = q05_session::AudioMute::new();

    let mut supervisor = q05_session::MediaSupervisor::start(
        video_tx,
        hid_tx,
        presence_tx,
        audio_tx,
        audio_mute.clone(),
    )?;

    let result = q05_ui::run(video_rx, presence_rx, hid_rx, audio_rx, audio_mute);
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

/// 从终端启动时附加父控制台；`--console` 时若没有父控制台则新建。
///
/// 已存在的标准句柄（例如 shell 重定向）保持不变。
/// 双击启动（无父控制台、无 `--console`）保持无控制台。
#[cfg(windows)]
fn setup_console(force: bool) -> bool {
    use windows::Win32::System::Console::{
        ATTACH_PARENT_PROCESS, AllocConsole, AttachConsole, STD_ERROR_HANDLE, STD_INPUT_HANDLE,
        STD_OUTPUT_HANDLE,
    };

    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.is_err() {
        // 输出已被 shell 重定向时（句柄有效）不额外弹控制台窗口。
        let redirected =
            console_handle_valid(STD_OUTPUT_HANDLE) && console_handle_valid(STD_ERROR_HANDLE);
        if !force && !redirected {
            return false;
        }
        if force && !redirected {
            unsafe {
                let _ = AllocConsole();
            }
        }
    }

    bind_console_handle(STD_OUTPUT_HANDLE, "CONOUT$");
    bind_console_handle(STD_ERROR_HANDLE, "CONOUT$");
    bind_console_handle(STD_INPUT_HANDLE, "CONIN$");
    true
}

/// Ctrl+C/Ctrl+Break 直接结束进程，避免 GUI 子系统下默认行为不可靠。
#[cfg(windows)]
fn install_ctrl_handler() {
    use windows::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler};
    use windows::Win32::System::Threading::ExitProcess;
    use windows::core::BOOL;

    unsafe extern "system" fn handler(ctrl_type: u32) -> BOOL {
        match ctrl_type {
            CTRL_C_EVENT | CTRL_BREAK_EVENT => unsafe { ExitProcess(0) },
            _ => false.into(),
        }
    }

    unsafe {
        let _ = SetConsoleCtrlHandler(Some(handler), true);
    }
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
