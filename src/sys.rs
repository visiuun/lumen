//! Autostart, daemon control, and the Windows gamma-range unlock.

use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::display::wide;

pub const DAEMON_CLASS: &str = "lumen.daemon";
/// Daemon: reload config and re-apply now.
pub const MSG_RELOAD: u32 = WM_APP + 1;
/// Daemon: stop enforcing for a few seconds (GUI is previewing). Sent as a heartbeat.
pub const MSG_PAUSE: u32 = WM_APP + 2;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const ICM_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\ICM";
const GAMMA_VALUE: &str = "GdiIcmGammaRange";

pub fn daemon_window() -> HWND {
    unsafe { FindWindowW(wide(DAEMON_CLASS).as_ptr(), null()) }
}

pub fn daemon_running() -> bool {
    !daemon_window().is_null()
}

pub fn post_daemon(msg: u32) {
    let h = daemon_window();
    if !h.is_null() {
        unsafe { PostMessageW(h, msg, 0, 0) };
    }
}

pub fn stop_daemon() {
    post_daemon(WM_CLOSE);
    let t = Instant::now();
    while daemon_running() && t.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub fn installed_exe() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| ".".into());
    base.join("lumen").join("lumen.exe")
}

fn reg_string(root: HKEY, key: &str, name: &str) -> Option<String> {
    let mut buf = [0u16; 1024];
    let mut len = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(
            root,
            wide(key).as_ptr(),
            wide(name).as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            buf.as_mut_ptr() as _,
            &mut len,
        )
    };
    (r == 0).then(|| String::from_utf16_lossy(&buf[..(len as usize / 2).saturating_sub(1)]))
}

pub fn autostart_enabled() -> bool {
    reg_string(HKEY_CURRENT_USER, RUN_KEY, "lumen").is_some()
}

/// Copy ourselves to a stable location, register it to run at logon, and start it.
pub fn enable_autostart() -> Result<(), String> {
    let dst = installed_exe();
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    if !same_file(&me, &dst) {
        stop_daemon();
        std::fs::create_dir_all(dst.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::copy(&me, &dst).map_err(|e| format!("copy to {}: {e}", dst.display()))?;
    }
    let cmd = wide(&format!("\"{}\" --daemon", dst.display()));
    let r = unsafe {
        let mut k: HKEY = null_mut();
        if RegCreateKeyExW(
            HKEY_CURRENT_USER,
            wide(RUN_KEY).as_ptr(),
            0,
            null(),
            0,
            KEY_SET_VALUE,
            null(),
            &mut k,
            null_mut(),
        ) != 0
        {
            return Err("cannot open Run key".into());
        }
        let r = RegSetValueExW(k, wide("lumen").as_ptr(), 0, REG_SZ, cmd.as_ptr() as _, (cmd.len() * 2) as u32);
        RegCloseKey(k);
        r
    };
    if r != 0 {
        return Err(format!("registry write failed ({r})"));
    }
    if !daemon_running() {
        std::process::Command::new(&dst)
            .arg("--daemon")
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn disable_autostart() {
    unsafe {
        let mut k: HKEY = null_mut();
        if RegOpenKeyExW(HKEY_CURRENT_USER, wide(RUN_KEY).as_ptr(), 0, KEY_SET_VALUE, &mut k) == 0 {
            RegDeleteValueW(k, wide("lumen").as_ptr());
            RegCloseKey(k);
        }
    }
    stop_daemon();
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Without this, Windows refuses gamma ramps that stray far from identity.
pub fn gamma_unlocked() -> bool {
    let mut v = 0u32;
    let mut len = 4u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            wide(ICM_KEY).as_ptr(),
            wide(GAMMA_VALUE).as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            &mut v as *mut _ as _,
            &mut len,
        )
    };
    r == 0 && v >= 256
}

/// Runs in the elevated child (`--unlock-gamma`).
pub fn write_gamma_unlock() -> bool {
    unsafe {
        let mut k: HKEY = null_mut();
        if RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            wide(ICM_KEY).as_ptr(),
            0,
            null(),
            0,
            KEY_SET_VALUE,
            null(),
            &mut k,
            null_mut(),
        ) != 0
        {
            return false;
        }
        let v: u32 = 256;
        let r = RegSetValueExW(k, wide(GAMMA_VALUE).as_ptr(), 0, REG_DWORD, &v as *const _ as _, 4);
        RegCloseKey(k);
        r == 0
    }
}

/// UAC prompt → relaunch ourselves elevated to write the HKLM value.
pub fn request_gamma_unlock() -> bool {
    let Ok(me) = std::env::current_exe() else { return false };
    let r = unsafe {
        ShellExecuteW(
            null_mut(),
            wide("runas").as_ptr(),
            wide(&me.to_string_lossy()).as_ptr(),
            wide("--unlock-gamma").as_ptr(),
            null(),
            SW_HIDE,
        )
    };
    r as isize > 32
}

/// GUI-subsystem binaries have no console; borrow the parent's for CLI output.
pub fn attach_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}
