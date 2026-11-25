//! 低级键盘钩子：透传期间拦截 Windows 会抢先响应的系统键。
//!
//! 只在本窗口是前台窗口且输入透传开启（`active`）时生效，其余情况一律放行。
//! 拦截范围：
//! - Win 键本身，以及按住 Win 时的所有非修饰键（开始菜单、Win+E/R/D/L 等）；
//! - PrintScreen（截图工具）；
//! - Alt+Tab / Alt+Esc（任务切换）；
//! - Ctrl+Esc、Ctrl+Shift+Esc（开始菜单 / 任务管理器）。
//!
//! 普通键与 Ctrl/Shift/Alt 仍走 GPUI 事件路径；Ctrl+Alt+Del 是安全注意序列，
//! 由系统直接处理，任何钩子都无法拦截。
//!
//! 钩子回调运行在安装线程（GPUI 主线程）上，只做按键判断与无阻塞发送，
//! 具体状态更新与 HID 发送在 `VideoView` 的异步任务里完成。

use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use async_channel::Sender;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetForegroundWindow, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_ALTDOWN,
    SetWindowsHookExW, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

use crate::HookKeyEvent;

static HOOK_TX: OnceLock<Sender<HookKeyEvent>> = OnceLock::new();
static HOOK_ACTIVE: OnceLock<Arc<AtomicBool>> = OnceLock::new();
static OUR_HWND: AtomicIsize = AtomicIsize::new(0);
/// Win 键是否处于按下状态；即使钩子未激活也持续跟踪。
static WIN_DOWN: AtomicBool = AtomicBool::new(false);
/// 钩子是否安装成功；失败时 Win 键仍走 GPUI。
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// 钩子是否安装成功。
pub(crate) fn is_installed() -> bool {
    INSTALLED.load(Ordering::Relaxed)
}

/// 在本窗口上安装全局低级键盘钩子。
///
/// 钩子跟随进程生命周期，不主动卸载（进程退出时由系统清理）。
pub(crate) fn install(hwnd: HWND, tx: Sender<HookKeyEvent>, active: Arc<AtomicBool>) {
    let _ = HOOK_TX.set(tx);
    let _ = HOOK_ACTIVE.set(active);
    OUR_HWND.store(hwnd.0 as isize, Ordering::Relaxed);

    let Ok(module) = (unsafe { GetModuleHandleW(None) }) else {
        return;
    };
    let installed = unsafe {
        SetWindowsHookExW(
            WH_KEYBOARD_LL,
            Some(hook_proc),
            Some(HINSTANCE(module.0)),
            0,
        )
    };
    match installed {
        Ok(_) => {
            INSTALLED.store(true, Ordering::Relaxed);
            tracing::info!("system keyboard hook installed");
        }
        Err(err) => {
            INSTALLED.store(false, Ordering::Relaxed);
            tracing::warn!(error = %err, "system keyboard hook install failed");
        }
    }
}

unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let pass = || unsafe { CallNextHookEx(None, code, wparam, lparam) };

    if code != HC_ACTION as i32 {
        return pass();
    }

    let pressed = match wparam.0 as u32 {
        WM_KEYDOWN | WM_SYSKEYDOWN => true,
        WM_KEYUP | WM_SYSKEYUP => false,
        _ => return pass(),
    };
    let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
    let vk = info.vkCode as u16;
    let is_win = is_win_key(vk);

    // 无论钩子是否生效，都保持 Win 状态准确，避免重新启用时残留。
    if is_win {
        WIN_DOWN.store(pressed, Ordering::Relaxed);
    }

    let active = HOOK_ACTIVE
        .get()
        .is_some_and(|flag| flag.load(Ordering::Relaxed));
    let foreground = unsafe { GetForegroundWindow() }.0 as isize;
    if !active || OUR_HWND.load(Ordering::Relaxed) != foreground {
        return pass();
    }

    let alt = (info.flags.0 & LLKHF_ALTDOWN.0) != 0 || held(0x12) || held(0xA4) || held(0xA5);
    let ctrl = held(0x11) || held(0xA2) || held(0xA3);
    let win_down = WIN_DOWN.load(Ordering::Relaxed);
    if !should_intercept(vk, win_down, alt, ctrl) {
        return pass();
    }

    let Some(tx) = HOOK_TX.get() else {
        return pass();
    };
    let modifiers = modifier_bits(if is_win { pressed } else { win_down });
    if is_win {
        let _ = tx.try_send(HookKeyEvent::Modifiers(modifiers));
    } else if let Some(usage) = usage_for_vk(vk) {
        // 映射不到也拦截：宁可丢键，也不要让 Windows 触发本地行为。
        let _ = tx.try_send(HookKeyEvent::Key {
            usage,
            pressed,
            modifiers,
        });
    }
    LRESULT(1)
}

fn is_win_key(vk: u16) -> bool {
    matches!(vk, 0x5B | 0x5C)
}

/// 是否拦截该按键。
fn should_intercept(vk: u16, win_down: bool, alt: bool, ctrl: bool) -> bool {
    // 修饰键与 CapsLock 由 GPUI 处理：前者的修饰位、后者的锁定状态都要
    // 跟本机保持一致，钩子只负责转发它们引发的后续变化。
    let passthrough = is_win_key(vk) || matches!(vk, 0x10..=0x12 | 0xA0..=0xA5) || vk == 0x14; // CapsLock
    is_win_key(vk)
        // 按住 Win 时，其余非修饰键都交给目标，避免触发 Win 组合。
        || (win_down && !passthrough)
        // PrintScreen（VK_SNAPSHOT）。
        || vk == 0x2C
        // Alt+Tab。
        || (vk == 0x09 && alt)
        // Alt+Esc、Ctrl+Esc、Ctrl+Shift+Esc。
        || (vk == 0x1B && (alt || ctrl))
}

/// 当前 HID 修饰键位图；Win 位由调用方显式给出（钩子自己跟踪）。
fn modifier_bits(platform: bool) -> u8 {
    let mut bits = 0u8;
    if held(0x11) || held(0xA2) || held(0xA3) {
        bits |= 0x01;
    }
    if held(0x10) || held(0xA0) || held(0xA1) {
        bits |= 0x02;
    }
    if held(0x12) || held(0xA4) || held(0xA5) {
        bits |= 0x04;
    }
    if platform {
        bits |= 0x08;
    }
    bits
}

fn held(vk: u16) -> bool {
    (unsafe { GetAsyncKeyState(vk as i32) } as u16 & 0x8000) != 0
}

/// Windows 虚拟键码 → HID usage（美式布局）。
fn usage_for_vk(vk: u16) -> Option<u8> {
    Some(match vk {
        0x08 => 0x2A,                            // Backspace
        0x09 => 0x2B,                            // Tab
        0x0D => 0x28,                            // Enter
        0x13 => 0x48,                            // Pause
        0x14 => 0x39,                            // CapsLock
        0x1B => 0x29,                            // Esc
        0x20 => 0x2C,                            // Space
        0x21 => 0x4B,                            // PageUp
        0x22 => 0x4E,                            // PageDown
        0x23 => 0x4D,                            // End
        0x24 => 0x4A,                            // Home
        0x25 => 0x50,                            // Left
        0x26 => 0x52,                            // Up
        0x27 => 0x4F,                            // Right
        0x28 => 0x51,                            // Down
        0x2C => 0x46,                            // PrintScreen
        0x2D => 0x49,                            // Insert
        0x2E => 0x4C,                            // Delete
        0x30 => 0x27,                            // 0
        0x31..=0x39 => 0x1E + (vk as u8 - 0x31), // 1..9
        0x41..=0x5A => 0x04 + (vk as u8 - 0x41), // A..Z
        0x5D => 0x65,                            // Menu
        0x60..=0x69 => {
            [0x62, 0x59, 0x5A, 0x5B, 0x5C, 0x5D, 0x5E, 0x5F, 0x60, 0x61][(vk - 0x60) as usize]
        } // 小键盘 0..9
        0x6A => 0x55,                            // 小键盘 *
        0x6B => 0x57,                            // 小键盘 +
        0x6D => 0x56,                            // 小键盘 -
        0x6E => 0x63,                            // 小键盘 .
        0x6F => 0x54,                            // 小键盘 /
        0x70..=0x87 => {
            // F1..F24
            let offset = vk - 0x70;
            if offset < 12 {
                0x3A + offset as u8
            } else {
                0x68 + (offset - 12) as u8
            }
        }
        0x90 => 0x53, // NumLock
        0x91 => 0x47, // ScrollLock
        0xBA => 0x33, // ;
        0xBB => 0x2E, // =
        0xBC => 0x36, // ,
        0xBD => 0x2D, // -
        0xBE => 0x37, // .
        0xBF => 0x38, // /
        0xC0 => 0x35, // `
        0xDB => 0x2F, // [
        0xDC => 0x31, // \
        0xDD => 0x30, // ]
        0xDE => 0x34, // '
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intercepts_system_keys() {
        // Win 键本身。
        assert!(should_intercept(0x5B, false, false, false));
        assert!(should_intercept(0x5C, false, false, false));
        // 按住 Win 时的普通键；修饰键放行给 GPUI。
        assert!(should_intercept(0x45, true, false, false)); // Win+E
        assert!(!should_intercept(0x10, true, false, false)); // Shift
        assert!(!should_intercept(0x41, false, false, false)); // 普通 A
        // PrintScreen。
        assert!(should_intercept(0x2C, false, false, false));
        // Alt+Tab，但普通 Tab 放行。
        assert!(should_intercept(0x09, false, true, false));
        assert!(!should_intercept(0x09, false, false, false));
        // Alt+Esc、Ctrl+Esc、Ctrl+Shift+Esc。
        assert!(should_intercept(0x1B, false, true, false));
        assert!(should_intercept(0x1B, false, false, true));
        assert!(!should_intercept(0x1B, false, false, false));
    }

    #[test]
    fn vk_maps_to_hid_usage() {
        assert_eq!(usage_for_vk(0x41), Some(0x04)); // A
        assert_eq!(usage_for_vk(0x45), Some(0x08)); // E
        assert_eq!(usage_for_vk(0x31), Some(0x1E)); // 1
        assert_eq!(usage_for_vk(0x30), Some(0x27)); // 0
        assert_eq!(usage_for_vk(0x2C), Some(0x46)); // PrintScreen
        assert_eq!(usage_for_vk(0x09), Some(0x2B)); // Tab
        assert_eq!(usage_for_vk(0x1B), Some(0x29)); // Esc
        assert_eq!(usage_for_vk(0x70), Some(0x3A)); // F1
        assert_eq!(usage_for_vk(0x7B), Some(0x45)); // F12
        assert_eq!(usage_for_vk(0x7C), Some(0x68)); // F13
        assert_eq!(usage_for_vk(0x87), Some(0x73)); // F24
        assert_eq!(usage_for_vk(0x60), Some(0x62)); // 小键盘 0
        assert_eq!(usage_for_vk(0x6A), Some(0x55)); // 小键盘 *
        assert_eq!(usage_for_vk(0xBA), Some(0x33)); // ;
        assert_eq!(usage_for_vk(0x5B), None); // Win 是修饰键，不作为 usage
    }
}
