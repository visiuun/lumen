//! NVIDIA Digital Vibrance (saturation applied in the GPU's display pipeline).
//! nvapi64.dll ships with the driver; functions are resolved by interface ID.

use std::ffi::c_void;
use std::sync::OnceLock;

use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

use crate::display::wide;

type QueryInterface = unsafe extern "C" fn(u32) -> *const c_void;
type Initialize = unsafe extern "C" fn() -> i32;
type GetHandle = unsafe extern "C" fn(*const u8, *mut *mut c_void) -> i32;
type GetDvc = unsafe extern "C" fn(*mut c_void, u32, *mut DvcInfo) -> i32;
type SetDvc = unsafe extern "C" fn(*mut c_void, u32, u32) -> i32;

const ID_INITIALIZE: u32 = 0x0150_E828;
const ID_GET_ASSOCIATED_DISPLAY_HANDLE: u32 = 0x35C2_9134;
const ID_GET_DVC_INFO: u32 = 0x4085_DE45;
const ID_SET_DVC_LEVEL: u32 = 0x1724_09B4;

#[repr(C)]
#[derive(Default)]
struct DvcInfo {
    version: u32,
    current: u32,
    min: u32,
    max: u32,
}

struct Api {
    handle: GetHandle,
    get: GetDvc,
    set: SetDvc,
}

fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| unsafe {
        let dll = if cfg!(target_pointer_width = "64") {
            "nvapi64.dll"
        } else {
            "nvapi.dll"
        };
        let lib = LoadLibraryW(wide(dll).as_ptr());
        if lib.is_null() {
            return None;
        }
        let qi = std::mem::transmute::<unsafe extern "system" fn() -> isize, QueryInterface>(GetProcAddress(lib, c"nvapi_QueryInterface".as_ptr() as _)?);
        let f = |id| {
            let p = qi(id);
            (!p.is_null()).then_some(p)
        };
        let init = std::mem::transmute::<*const c_void, Initialize>(f(ID_INITIALIZE)?);
        if init() != 0 {
            return None;
        }
        Some(Api {
            handle: std::mem::transmute::<*const c_void, GetHandle>(f(ID_GET_ASSOCIATED_DISPLAY_HANDLE)?),
            get: std::mem::transmute::<*const c_void, GetDvc>(f(ID_GET_DVC_INFO)?),
            set: std::mem::transmute::<*const c_void, SetDvc>(f(ID_SET_DVC_LEVEL)?),
        })
    })
    .as_ref()
}

fn display_handle(api: &Api, gdi: &str) -> Option<*mut c_void> {
    let name: Vec<u8> = gdi.bytes().chain(Some(0)).collect();
    let mut h = std::ptr::null_mut();
    (unsafe { (api.handle)(name.as_ptr(), &mut h) } == 0 && !h.is_null()).then_some(h)
}

/// (current, max). Level 0 is the driver default ("50%" in NVIDIA Control Panel).
pub fn vibrance(gdi: &str) -> Option<(u32, u32)> {
    let api = api()?;
    let h = display_handle(api, gdi)?;
    let mut info = DvcInfo {
        version: size_of::<DvcInfo>() as u32 | (1 << 16),
        ..Default::default()
    };
    (unsafe { (api.get)(h, 0, &mut info) } == 0).then_some((info.current, info.max))
}

pub fn set_vibrance(gdi: &str, level: u32) -> bool {
    let Some(api) = api() else { return false };
    let Some(h) = display_handle(api, gdi) else { return false };
    unsafe { (api.set)(h, 0, level) == 0 }
}
