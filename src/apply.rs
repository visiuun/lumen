use crate::config::{Config, Curve, Hardware, Profile};
use crate::display::{self, Ddc, Display, vcp};
use crate::nvapi;

/// Write every configured hardware value. Returns how many writes succeeded.
pub fn hardware(ddc: &Ddc, hw: &Hardware) -> usize {
    let mut n = 0;
    if hw.gains_set() && ddc.get(vcp::COLOR_PRESET).is_none_or(|(c, _)| c != vcp::PRESET_USER1) {
        ddc.set(vcp::COLOR_PRESET, vcp::PRESET_USER1);
    }
    let writes = [
        (vcp::BRIGHTNESS, hw.brightness),
        (vcp::CONTRAST, hw.contrast),
        (vcp::RED_GAIN, hw.red),
        (vcp::GREEN_GAIN, hw.green),
        (vcp::BLUE_GAIN, hw.blue),
    ];
    for (code, v) in writes
        .into_iter()
        .filter_map(|(c, v)| Some((c, v?)))
        .chain(hw.extra.iter().copied())
    {
        n += ddc.set(code, v) as usize;
    }
    n
}

/// True if the monitor's live values differ from what we want.
pub fn hardware_drifted(ddc: &Ddc, hw: &Hardware) -> bool {
    [(vcp::BRIGHTNESS, hw.brightness), (vcp::CONTRAST, hw.contrast)]
        .into_iter()
        .filter_map(|(c, v)| Some((c, v?)))
        .any(|(c, want)| ddc.get(c).is_some_and(|(cur, _)| cur != want))
}

pub fn curve(d: &Display, c: &Curve) -> bool {
    let ramp = if c.enabled { c.ramp() } else { display::identity_ramp() };
    display::set_ramp(&d.gdi, &ramp)
}

/// GPU-side state: LUT curve + saturation. Cheap, so it can run every second.
/// With `only_if_changed`, reads back first and only writes on drift.
pub fn signal(d: &Display, p: &Profile, only_if_changed: bool) {
    if p.curve.enabled {
        let want = p.curve.ramp();
        if !only_if_changed || display::get_ramp(&d.gdi).is_none_or(|cur| !display::ramps_match(&cur, &want)) {
            display::set_ramp(&d.gdi, &want);
        }
    }
    if let Some(v) = p.vibrance
        && (!only_if_changed || nvapi::vibrance(&d.gdi).is_some_and(|(cur, _)| cur != v))
    {
        nvapi::set_vibrance(&d.gdi, v);
    }
}

/// Apply curve and saturation to every connected display that has a profile.
pub fn signal_all(cfg: &Config) {
    for d in display::displays() {
        if let Some(p) = cfg.profiles.get(&d.id) {
            signal(&d, p, false);
        }
    }
}

/// Write hardware settings to every connected display that has a profile. Slow.
pub fn hardware_all(cfg: &Config) {
    for d in display::displays() {
        if let Some(p) = cfg.profiles.get(&d.id)
            && let Some(ddc) = Ddc::open(&d.gdi)
            && hardware(&ddc, &p.hardware) > 0
        {
            ddc.save();
        }
    }
}
