#![windows_subsystem = "windows"]

mod apply;
mod config;
mod daemon;
mod display;
mod icon;
mod nvapi;
mod patterns;
mod sys;
mod ui;

const HELP: &str = "\
lumen: hardware monitor calibration that stays applied

USAGE
    lumen                   open the app
    lumen --calibrate [N]   open the calibration wizard (optionally at step N, 0-6)
    lumen --apply           apply the saved calibration once and exit
    lumen --daemon          run the background enforcer (started automatically by Keep applied)
    lumen --probe           print what lumen can see and control on each display
    lumen --reset           stop the enforcer and restore a neutral GPU curve
    lumen --uninstall       remove autostart, the installed copy and all settings
    lumen --help            show this help
";

fn main() {
    let arg = std::env::args().nth(1);
    match arg.as_deref() {
        Some("--daemon") => daemon::run(),
        Some("--unlock-gamma") => std::process::exit(if sys::write_gamma_unlock() { 0 } else { 1 }),
        Some("--calibrate") => ui::run(Some(std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(0))),
        Some("--apply") => {
            let cfg = config::Config::load();
            apply::signal_all(&cfg);
            apply::hardware_all(&cfg);
        }
        Some("--reset") => {
            sys::stop_daemon();
            reset_signal();
        }
        Some("--uninstall") => {
            sys::attach_console();
            uninstall();
        }
        Some("--probe") => {
            sys::attach_console();
            probe();
        }
        Some("--help" | "-h" | "/?") => {
            sys::attach_console();
            print!("{HELP}");
        }
        Some(other) => {
            sys::attach_console();
            eprint!("unknown option: {other}\n\n{HELP}");
            std::process::exit(2);
        }
        None => ui::run(None),
    }
}

/// Neutral LUT everywhere, and neutral vibrance wherever lumen had changed it.
fn reset_signal() {
    let cfg = config::Config::load();
    for d in display::displays() {
        display::set_ramp(&d.gdi, &display::identity_ramp());
        if cfg.profiles.get(&d.id).is_some_and(|p| p.vibrance.is_some()) {
            nvapi::set_vibrance(&d.gdi, 0);
        }
    }
}

fn uninstall() {
    reset_signal();
    sys::disable_autostart();
    let installed = sys::installed_exe();
    let me = std::env::current_exe().ok().and_then(|p| p.canonicalize().ok());
    if installed.exists() && installed.canonicalize().ok() != me {
        let _ = std::fs::remove_file(&installed);
        if let Some(dir) = installed.parent() {
            let _ = std::fs::remove_dir(dir);
        }
    }
    let _ = std::fs::remove_dir_all(config::dir());
    println!("lumen removed. Monitor hardware settings stay as they are; reset them from the monitor menu if needed.");
}

/// Read-only diagnostics: what lumen can see and control.
fn probe() {
    use display::{Ddc, vcp};
    println!("gamma range unlocked: {}", sys::gamma_unlocked());
    println!("enforcer running:     {}", sys::daemon_running());
    for d in display::displays() {
        println!("\n{} [{}]{}", d.name, d.gdi, if d.primary { " primary" } else { "" });
        println!("  id: {}", d.id);
        println!(
            "  gpu gamma ramp:   {}",
            if display::get_ramp(&d.gdi).is_some() { "ok" } else { "unavailable" }
        );
        match nvapi::vibrance(&d.gdi) {
            Some((cur, max)) => println!("  nvidia vibrance:  {cur}/{max}"),
            None => println!("  nvidia vibrance:  unavailable"),
        }
        let Some(ddc) = Ddc::open(&d.gdi) else {
            println!("  DDC/CI:           no physical monitor handle");
            continue;
        };
        let codes = [
            ("brightness", vcp::BRIGHTNESS),
            ("contrast", vcp::CONTRAST),
            ("colour preset", vcp::COLOR_PRESET),
            ("red gain", vcp::RED_GAIN),
            ("green gain", vcp::GREEN_GAIN),
            ("blue gain", vcp::BLUE_GAIN),
            ("saturation (8A)", 0x8A),
            ("display mode (DC)", 0xDC),
        ];
        for (name, code) in codes {
            match ddc.get(code) {
                Some((cur, max)) => println!("  {name:18}{cur}/{max}"),
                None => println!("  {name:18}no reply"),
            }
        }
    }
}
