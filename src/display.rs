//! Display enumeration, DDC/CI (writes into the monitor's own settings) and
//! GPU gamma ramps (the video card's hardware LUT, not an overlay).

use std::collections::HashMap;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::thread::sleep;
use std::time::Duration;

use windows_sys::Win32::Devices::Display::*;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::ColorSystem::{GetDeviceGammaRamp, SetDeviceGammaRamp};

pub type Ramp = [[u16; 256]; 3];

pub mod vcp {
    pub const BRIGHTNESS: u8 = 0x10;
    pub const CONTRAST: u8 = 0x12;
    pub const COLOR_PRESET: u8 = 0x14;
    pub const RED_GAIN: u8 = 0x16;
    pub const GREEN_GAIN: u8 = 0x18;
    pub const BLUE_GAIN: u8 = 0x1A;
    /// MCCS colour preset value for "User 1". RGB gains only take effect in this mode.
    pub const PRESET_USER1: u32 = 0x0B;
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn from_wide(b: &[u16]) -> String {
    let n = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf16_lossy(&b[..n])
}

#[derive(Clone, Debug)]
pub struct Display {
    /// GDI name, e.g. `\\.\DISPLAY1`. Can change across driver reinstalls.
    pub gdi: String,
    /// Stable identity (monitor device path, derived from EDID + port).
    pub id: String,
    /// Friendly name from EDID, e.g. `Q24G4`.
    pub name: String,
    pub primary: bool,
}

/// Maps GDI source name -> (device path, friendly name) via the DisplayConfig API.
fn target_names() -> HashMap<String, (String, String)> {
    let mut out = HashMap::new();
    unsafe {
        let (mut np, mut nm) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm) != 0 {
            return out;
        }
        let mut paths = vec![zeroed::<DISPLAYCONFIG_PATH_INFO>(); np as usize];
        let mut modes = vec![zeroed::<DISPLAYCONFIG_MODE_INFO>(); nm as usize];
        if QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut np,
            paths.as_mut_ptr(),
            &mut nm,
            modes.as_mut_ptr(),
            null_mut(),
        ) != 0
        {
            return out;
        }
        for p in &paths[..np as usize] {
            let mut src: DISPLAYCONFIG_SOURCE_DEVICE_NAME = zeroed();
            src.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            src.header.size = size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
            src.header.adapterId = p.sourceInfo.adapterId;
            src.header.id = p.sourceInfo.id;
            let mut tgt: DISPLAYCONFIG_TARGET_DEVICE_NAME = zeroed();
            tgt.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
            tgt.header.size = size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
            tgt.header.adapterId = p.targetInfo.adapterId;
            tgt.header.id = p.targetInfo.id;
            if DisplayConfigGetDeviceInfo(&mut src.header) == 0 && DisplayConfigGetDeviceInfo(&mut tgt.header) == 0 {
                out.insert(
                    from_wide(&src.viewGdiDeviceName),
                    (from_wide(&tgt.monitorDevicePath), from_wide(&tgt.monitorFriendlyDeviceName)),
                );
            }
        }
    }
    out
}

fn hmonitors() -> Vec<(HMONITOR, String, bool)> {
    unsafe extern "system" fn cb(h: HMONITOR, _: HDC, _: *mut RECT, lp: LPARAM) -> windows_sys::core::BOOL {
        unsafe { (*(lp as *mut Vec<HMONITOR>)).push(h) };
        1
    }
    let mut hs: Vec<HMONITOR> = Vec::new();
    unsafe {
        EnumDisplayMonitors(null_mut(), null(), Some(cb), &mut hs as *mut _ as LPARAM);
    }
    hs.into_iter()
        .filter_map(|h| unsafe {
            let mut mi: MONITORINFOEXW = zeroed();
            mi.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
            (GetMonitorInfoW(h, &mut mi as *mut _ as *mut MONITORINFO) != 0)
                .then(|| (h, from_wide(&mi.szDevice), mi.monitorInfo.dwFlags & 1 != 0))
        })
        .collect()
}

pub fn displays() -> Vec<Display> {
    let names = target_names();
    let mut v: Vec<Display> = hmonitors()
        .into_iter()
        .map(|(_, gdi, primary)| {
            let (id, name) = names.get(&gdi).cloned().unwrap_or_default();
            Display {
                id: if id.is_empty() { gdi.clone() } else { id },
                name: if name.is_empty() {
                    gdi.trim_start_matches(r"\\.\").to_string()
                } else {
                    name
                },
                gdi,
                primary,
            }
        })
        .collect();
    v.sort_by_key(|d| (!d.primary, d.gdi.clone()));
    v
}

/// Physical monitor handles for one display; closed on drop.
pub struct Ddc(Vec<PHYSICAL_MONITOR>);

impl Ddc {
    pub fn open(gdi: &str) -> Option<Ddc> {
        let (h, _, _) = hmonitors().into_iter().find(|(_, g, _)| g == gdi)?;
        unsafe {
            let mut n = 0u32;
            if GetNumberOfPhysicalMonitorsFromHMONITOR(h, &mut n) == 0 || n == 0 {
                return None;
            }
            let mut v = vec![zeroed::<PHYSICAL_MONITOR>(); n as usize];
            if GetPhysicalMonitorsFromHMONITOR(h, n, v.as_mut_ptr()) == 0 {
                return None;
            }
            Some(Ddc(v))
        }
    }

    /// (current, max). DDC/CI is a slow, lossy I2C link, so retry a few times.
    pub fn get(&self, code: u8) -> Option<(u32, u32)> {
        let h = self.0.first()?.hPhysicalMonitor;
        for _ in 0..3 {
            let (mut cur, mut max) = (0u32, 0u32);
            if unsafe { GetVCPFeatureAndVCPFeatureReply(h, code, null_mut(), &mut cur, &mut max) } != 0 {
                return Some((cur, max));
            }
            sleep(Duration::from_millis(40));
        }
        None
    }

    pub fn set(&self, code: u8, value: u32) -> bool {
        let mut ok = false;
        for m in &self.0 {
            for _ in 0..3 {
                if unsafe { SetVCPFeature(m.hPhysicalMonitor, code, value) } != 0 {
                    ok = true;
                    break;
                }
                sleep(Duration::from_millis(40));
            }
            // MCCS requires >=50 ms between writes or many scalers silently drop them.
            sleep(Duration::from_millis(50));
        }
        ok
    }

    /// Ask the monitor to commit current settings to its NVRAM.
    pub fn save(&self) {
        for m in &self.0 {
            unsafe { SaveCurrentSettings(m.hPhysicalMonitor) };
        }
    }
}

impl Drop for Ddc {
    fn drop(&mut self) {
        unsafe { DestroyPhysicalMonitors(self.0.len() as u32, self.0.as_ptr()) };
    }
}

fn with_dc<T>(gdi: &str, f: impl FnOnce(HDC) -> T) -> Option<T> {
    unsafe {
        let hdc = CreateDCW(wide("DISPLAY").as_ptr(), wide(gdi).as_ptr(), null(), null());
        if hdc.is_null() {
            return None;
        }
        let r = f(hdc);
        DeleteDC(hdc);
        Some(r)
    }
}

pub fn set_ramp(gdi: &str, ramp: &Ramp) -> bool {
    with_dc(gdi, |dc| unsafe { SetDeviceGammaRamp(dc, ramp.as_ptr() as _) } != 0).unwrap_or(false)
}

pub fn get_ramp(gdi: &str) -> Option<Ramp> {
    with_dc(gdi, |dc| {
        let mut r: Ramp = [[0; 256]; 3];
        (unsafe { GetDeviceGammaRamp(dc, r.as_mut_ptr() as _) } != 0).then_some(r)
    })
    .flatten()
}

pub fn identity_ramp() -> Ramp {
    let mut r = [[0u16; 256]; 3];
    for ch in &mut r {
        for (i, v) in ch.iter_mut().enumerate() {
            *v = (i as u16) * 257;
        }
    }
    r
}

/// Drivers quantise the LUT (often to 10-12 bits), so compare loosely.
pub fn ramps_match(a: &Ramp, b: &Ramp) -> bool {
    a.iter().flatten().zip(b.iter().flatten()).all(|(x, y)| x.abs_diff(*y) <= 512)
}
