//! Background enforcer. A hidden top-level window (message-only windows miss
//! broadcasts) that re-applies the calibration whenever Windows, a game, or the
//! NVIDIA driver resets it.

use std::cell::RefCell;
use std::ptr::{null, null_mut};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Power::RegisterPowerSettingNotification;
use windows_sys::Win32::System::RemoteDesktop::{NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification};
use windows_sys::Win32::System::SystemServices::GUID_CONSOLE_DISPLAY_STATE;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::config::{self, Config};
use crate::display::{self, Ddc};
use crate::{apply, sys};

const T_WATCH: usize = 1; // gamma ramp + config file watch
const T_SETTLE_A: usize = 2; // after a display event, once things settle
const T_SETTLE_B: usize = 3; // ...and again later, for slow monitors / driver installs
const T_HARDWARE: usize = 4; // DDC drift check

const WTS_CONSOLE_CONNECT: WPARAM = 1;
const WTS_SESSION_UNLOCK: WPARAM = 8;

struct State {
    cfg: Config,
    mtime: Option<SystemTime>,
    paused_until: Instant,
    ddc_busy: Arc<AtomicBool>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

#[repr(C)]
struct PowerSetting {
    guid: windows_sys::core::GUID,
    len: u32,
    data: [u8; 4],
}

pub fn run() {
    // Named mutex closes the race where two instances start at once (e.g. logon + manual).
    let mutex = unsafe { CreateMutexW(null(), 1, display::wide(r"Local\lumen.daemon").as_ptr()) };
    if mutex.is_null() || unsafe { GetLastError() } == ERROR_ALREADY_EXISTS || sys::daemon_running() {
        return;
    }
    STATE.with_borrow_mut(|s| {
        *s = Some(State {
            cfg: Config::load(),
            mtime: config::mtime(),
            paused_until: Instant::now(),
            ddc_busy: Arc::new(AtomicBool::new(false)),
        })
    });

    unsafe {
        let class = display::wide(sys::DAEMON_CLASS);
        let hinst = GetModuleHandleW(null());
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class.as_ptr(),
            class.as_ptr(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            hinst,
            null(),
        );
        if hwnd.is_null() {
            return;
        }
        WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION);
        RegisterPowerSettingNotification(hwnd, &GUID_CONSOLE_DISPLAY_STATE, DEVICE_NOTIFY_WINDOW_HANDLE);

        full_reapply();
        SetTimer(hwnd, T_WATCH, 1000, None);
        SetTimer(hwnd, T_HARDWARE, 45_000, None);

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            DispatchMessageW(&msg);
        }
    }
}

fn schedule(hwnd: HWND) {
    // Re-arming resets the countdown, so bursts of events collapse into one pass.
    unsafe {
        SetTimer(hwnd, T_SETTLE_A, 1500, None);
        SetTimer(hwnd, T_SETTLE_B, 7000, None);
    }
}

fn paused() -> bool {
    STATE.with_borrow(|s| s.as_ref().is_some_and(|s| Instant::now() < s.paused_until))
}

fn reload_if_changed() {
    let m = config::mtime();
    let changed = STATE.with_borrow_mut(|s| {
        let s = s.as_mut().unwrap();
        if m != s.mtime {
            s.mtime = m;
            s.cfg = Config::load();
            true
        } else {
            false
        }
    });
    if changed {
        full_reapply();
    }
}

/// Gamma on this thread (fast), DDC on a worker (each write takes 50-100 ms).
fn full_reapply() {
    if paused() {
        return;
    }
    let (cfg, busy) = STATE.with_borrow(|s| {
        let s = s.as_ref().unwrap();
        (s.cfg.clone(), s.ddc_busy.clone())
    });
    apply::signal_all(&cfg);
    if !busy.swap(true, Ordering::AcqRel) {
        std::thread::spawn(move || {
            apply::hardware_all(&cfg);
            busy.store(false, Ordering::Release);
        });
    }
}

/// Cheap check every second: did something (fullscreen game, driver, colour
/// profile loader, NVIDIA panel) overwrite the LUT or vibrance? Put ours back.
fn enforce_gamma() {
    if paused() {
        return;
    }
    let cfg = STATE.with_borrow(|s| s.as_ref().unwrap().cfg.clone());
    for d in display::displays() {
        if let Some(p) = cfg.profiles.get(&d.id) {
            apply::signal(&d, p, true);
        }
    }
}

fn enforce_hardware() {
    if paused() {
        return;
    }
    let (cfg, busy) = STATE.with_borrow(|s| {
        let s = s.as_ref().unwrap();
        (s.cfg.clone(), s.ddc_busy.clone())
    });
    if !cfg.enforce_hardware || busy.swap(true, Ordering::AcqRel) {
        return;
    }
    std::thread::spawn(move || {
        for d in display::displays() {
            let Some(p) = cfg.profiles.get(&d.id) else { continue };
            if let Some(ddc) = Ddc::open(&d.gdi)
                && apply::hardware_drifted(&ddc, &p.hardware)
            {
                apply::hardware(&ddc, &p.hardware);
                ddc.save();
            }
        }
        busy.store(false, Ordering::Release);
    });
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_DISPLAYCHANGE | WM_DEVICECHANGE | WM_DWMCOMPOSITIONCHANGED => schedule(hwnd),
        WM_WTSSESSION_CHANGE if wp == WTS_SESSION_UNLOCK || wp == WTS_CONSOLE_CONNECT => schedule(hwnd),
        WM_POWERBROADCAST => {
            if wp == PBT_APMRESUMEAUTOMATIC as WPARAM {
                schedule(hwnd);
            } else if wp == PBT_POWERSETTINGCHANGE as WPARAM && lp != 0 {
                // Display came back on (1 = on, 2 = dimmed).
                let ps = unsafe { &*(lp as *const PowerSetting) };
                if ps.data[0] != 0 {
                    schedule(hwnd);
                }
            }
            return 1;
        }
        WM_TIMER => match wp {
            T_WATCH => {
                reload_if_changed();
                enforce_gamma();
            }
            T_SETTLE_A | T_SETTLE_B => {
                unsafe { KillTimer(hwnd, wp) };
                full_reapply();
            }
            T_HARDWARE => enforce_hardware(),
            _ => {}
        },
        sys::MSG_RELOAD => {
            STATE.with_borrow_mut(|s| {
                let s = s.as_mut().unwrap();
                s.cfg = Config::load();
                s.mtime = config::mtime();
                s.paused_until = Instant::now();
            });
            full_reapply();
        }
        sys::MSG_PAUSE => STATE.with_borrow_mut(|s| s.as_mut().unwrap().paused_until = Instant::now() + Duration::from_secs(5)),
        WM_DESTROY => unsafe { PostQuitMessage(0) },
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}
