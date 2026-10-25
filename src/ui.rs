use std::collections::{BTreeMap, HashMap};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, CornerRadius, Rect, RichText, Sense, Stroke, pos2, vec2};

use crate::config::{Config, Curve, Profile};
use crate::display::{self, Ddc, Display, vcp};
use crate::patterns::{self, Textures};
use crate::{apply, nvapi, sys};

const BG: Color32 = Color32::BLACK;
const DIM: Color32 = Color32::from_gray(110);
const FAINT: Color32 = Color32::from_gray(38);
const TEXT: Color32 = Color32::from_gray(225);

/// Usable desktop height in logical pixels (taskbar excluded). Queried before
/// the window exists, while the process is still DPI-unaware, so no scaling needed.
fn work_area_height() -> f32 {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::UI::WindowsAndMessaging::{SPI_GETWORKAREA, SystemParametersInfoW};
    let mut r: RECT = unsafe { std::mem::zeroed() };
    if unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut r as *mut _ as _, 0) } != 0 {
        (r.bottom - r.top) as f32
    } else {
        900.0
    }
}

pub fn run(calibrate: Option<usize>) {
    let height = (work_area_height() - 40.0).clamp(560.0, 900.0);
    let icon = egui::IconData {
        rgba: crate::icon::icon_rgba(64),
        width: 64,
        height: 64,
    };
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("lumen")
            .with_icon(icon)
            .with_inner_size([420.0, height])
            .with_decorations(false)
            .with_resizable(false),
        ..Default::default()
    };
    let result = eframe::run_native(
        "lumen",
        opts,
        Box::new(move |cc| {
            let mut app = App::new(&cc.egui_ctx);
            if let Some(step) = calibrate {
                app.cal = Some(step.min(STEPS.len() - 1));
                cc.egui_ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
            }
            Ok(Box::new(app))
        }),
    );
    if let Err(e) = result {
        let msg = display::wide(&format!(
            "lumen could not start its window:

{e}

The UI needs OpenGL 2.0 or newer. Update your graphics driver;              in a VM or Remote Desktop session there may be no OpenGL at all. Background mode (--daemon) still works."
        ));
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
                std::ptr::null_mut(),
                msg.as_ptr(),
                display::wide("lumen").as_ptr(),
                windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
            );
        }
    }
}

enum Job {
    Set(String, u8, u32),
    Save(String),
}

#[derive(Default)]
struct Probe {
    vals: BTreeMap<u8, (u32, u32)>,
}

struct App {
    cfg: Config,
    saved: Config,
    displays: Vec<Display>,
    sel: usize,
    probes: HashMap<String, Probe>,
    probe_rx: Receiver<(String, Probe)>,
    jobs: Option<Sender<Job>>,
    worker: Option<JoinHandle<()>>,
    changed_at: Option<Instant>,
    hw_touched: bool,
    last_ping: Instant,
    daemon: bool,
    autostart: bool,
    unlocked: bool,
    ramp_ok: bool,
    status: String,
    vcp_code: String,
    vcp_val: String,
    /// NVIDIA vibrance (current, max) per GDI name.
    vib: HashMap<String, (u32, u32)>,
    /// Calibration wizard step, when running.
    cal: Option<usize>,
    tex: Textures,
}

impl App {
    fn new(ctx: &egui::Context) -> Self {
        style(ctx);
        let cfg = Config::load();
        let displays = display::displays();

        let (ptx, probe_rx) = channel();
        for d in &displays {
            let (gdi, ptx, ctx) = (d.gdi.clone(), ptx.clone(), ctx.clone());
            std::thread::spawn(move || {
                let mut p = Probe::default();
                if let Some(ddc) = Ddc::open(&gdi) {
                    for c in [
                        vcp::BRIGHTNESS,
                        vcp::CONTRAST,
                        vcp::RED_GAIN,
                        vcp::GREEN_GAIN,
                        vcp::BLUE_GAIN,
                        vcp::COLOR_PRESET,
                    ] {
                        if let Some(v) = ddc.get(c) {
                            p.vals.insert(c, v);
                        }
                    }
                }
                let _ = ptx.send((gdi, p));
                ctx.request_repaint();
            });
        }

        let (jtx, jrx) = channel();
        let worker = std::thread::spawn(move || ddc_worker(jrx));

        sys::post_daemon(sys::MSG_PAUSE);
        let mut app = App {
            saved: cfg.clone(),
            cfg,
            displays,
            sel: 0,
            probes: HashMap::new(),
            probe_rx,
            jobs: Some(jtx),
            worker: Some(worker),
            changed_at: None,
            hw_touched: false,
            last_ping: Instant::now(),
            daemon: sys::daemon_running(),
            autostart: sys::autostart_enabled(),
            unlocked: sys::gamma_unlocked(),
            ramp_ok: true,
            status: String::new(),
            vcp_code: "DC".into(),
            vcp_val: "0".into(),
            vib: app_vibrance(),
            cal: None,
            tex: Textures::new(ctx),
        };
        app.push_curve();
        app
    }

    fn display(&self) -> Option<&Display> {
        self.displays.get(self.sel)
    }

    fn profile(&mut self) -> Option<&mut Profile> {
        let d = self.displays.get(self.sel)?;
        Some(self.cfg.profiles.entry(d.id.clone()).or_insert_with(|| Profile {
            name: d.name.clone(),
            ..Default::default()
        }))
    }

    fn push_curve(&mut self) {
        let Some(d) = self.display().cloned() else { return };
        let curve = self.profile().map(|p| p.curve.clone()).unwrap_or_default();
        self.ramp_ok = apply::curve(&d, &curve);
        self.changed_at = Some(Instant::now());
    }

    fn push_vibrance(&mut self, v: u32) {
        let Some(gdi) = self.display().map(|d| d.gdi.clone()) else { return };
        if nvapi::set_vibrance(&gdi, v)
            && let Some(e) = self.vib.get_mut(&gdi)
        {
            e.0 = v;
        }
        self.profile().unwrap().vibrance = Some(v);
        self.changed_at = Some(Instant::now());
    }

    fn push_vcp(&mut self, code: u8, v: u32) {
        let Some(gdi) = self.display().map(|d| d.gdi.clone()) else { return };
        if let Some(tx) = &self.jobs {
            if matches!(code, vcp::RED_GAIN | vcp::GREEN_GAIN | vcp::BLUE_GAIN) {
                let _ = tx.send(Job::Set(gdi.clone(), vcp::COLOR_PRESET, vcp::PRESET_USER1));
            }
            let _ = tx.send(Job::Set(gdi.clone(), code, v));
        }
        if let Some(e) = self.probes.get_mut(&gdi).and_then(|p| p.vals.get_mut(&code)) {
            e.0 = v;
        }
        self.hw_touched = true;
        self.changed_at = Some(Instant::now());
    }

    fn persist(&mut self) {
        if self.cfg != self.saved {
            match self.cfg.save() {
                Ok(()) => self.saved = self.cfg.clone(),
                Err(e) => self.status = format!("save failed: {e}"),
            }
        }
        if std::mem::take(&mut self.hw_touched)
            && let (Some(tx), Some(d)) = (&self.jobs, self.displays.get(self.sel))
        {
            let _ = tx.send(Job::Save(d.gdi.clone()));
        }
    }

    fn tick(&mut self, ctx: &egui::Context) {
        while let Ok((gdi, p)) = self.probe_rx.try_recv() {
            self.probes.insert(gdi, p);
        }
        if self.changed_at.is_some_and(|t| t.elapsed() > Duration::from_millis(700)) {
            self.changed_at = None;
            self.persist();
        }
        if self.last_ping.elapsed() > Duration::from_secs(2) {
            self.last_ping = Instant::now();
            sys::post_daemon(sys::MSG_PAUSE);
            self.daemon = sys::daemon_running();
            if !self.unlocked {
                self.unlocked = sys::gamma_unlocked();
            }
        }
        ctx.request_repaint_after(Duration::from_millis(500));
    }
}

fn app_vibrance() -> HashMap<String, (u32, u32)> {
    display::displays()
        .into_iter()
        .filter_map(|d| Some((d.gdi.clone(), nvapi::vibrance(&d.gdi)?)))
        .collect()
}

fn ddc_worker(rx: Receiver<Job>) {
    while let Ok(first) = rx.recv() {
        // Coalesce slider spam: only the latest value per (display, code) matters.
        let mut sets: Vec<(String, u8, u32)> = Vec::new();
        let mut saves: Vec<String> = Vec::new();
        for job in std::iter::once(first).chain(rx.try_iter()) {
            match job {
                Job::Set(g, c, v) => {
                    sets.retain(|(g2, c2, _)| !(g2 == &g && *c2 == c));
                    sets.push((g, c, v));
                }
                Job::Save(g) => saves.push(g),
            }
        }
        let mut gdis: Vec<&String> = sets.iter().map(|s| &s.0).chain(&saves).collect();
        gdis.sort();
        gdis.dedup();
        for gdi in gdis {
            let Some(ddc) = Ddc::open(gdi) else { continue };
            for (_, c, v) in sets.iter().filter(|s| &s.0 == gdi) {
                ddc.set(*c, *v);
            }
            if saves.contains(gdi) {
                ddc.save();
            }
        }
    }
}

fn style(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = BG;
    v.extreme_bg_color = Color32::from_gray(10);
    v.faint_bg_color = Color32::from_gray(8);
    v.override_text_color = Some(TEXT);
    v.selection.bg_fill = Color32::from_gray(235);
    v.selection.stroke = Stroke::new(1.0, Color32::BLACK);
    let r = CornerRadius::same(3);
    for (w, fill, fg) in [
        (&mut v.widgets.noninteractive, BG, DIM),
        (&mut v.widgets.inactive, Color32::from_gray(24), TEXT),
        (&mut v.widgets.hovered, Color32::from_gray(40), Color32::WHITE),
        (&mut v.widgets.active, Color32::from_gray(60), Color32::WHITE),
        (&mut v.widgets.open, Color32::from_gray(24), TEXT),
    ] {
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
        w.bg_stroke = Stroke::NONE;
        w.fg_stroke = Stroke::new(1.0, fg);
        w.corner_radius = r;
        w.expansion = 0.0;
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, FAINT);
    ctx.set_visuals(v);
    ctx.global_style_mut(|s| {
        s.spacing.item_spacing = vec2(8.0, 9.0);
        s.spacing.slider_rail_height = 3.0;
        s.spacing.interact_size.y = 20.0;
        s.spacing.button_padding = vec2(10.0, 5.0);
    });
}

fn heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(10.0);
    ui.label(RichText::new(text).size(10.5).color(DIM).extra_letter_spacing(1.6));
    ui.add_space(2.0);
}

/// label | slider | value. Returns true when the user moved it.
fn row(
    ui: &mut egui::Ui,
    label: &str,
    v: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    enabled: bool,
    fmt: impl Fn(f32) -> String,
) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.add_enabled_ui(enabled, |ui| {
            ui.allocate_ui_with_layout(vec2(92.0, 20.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.set_min_width(92.0);
                ui.label(RichText::new(label).color(if enabled { TEXT } else { FAINT }));
            });
            ui.spacing_mut().slider_width = ui.available_width() - 52.0;
            changed = ui.add(egui::Slider::new(v, range).show_value(false).trailing_fill(true)).changed();
            let txt = if enabled { fmt(*v) } else { "—".into() };
            ui.allocate_ui_with_layout(vec2(44.0, 20.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.set_min_width(44.0);
                ui.label(RichText::new(txt).monospace().size(12.0).color(DIM));
            });
        });
    });
    changed
}

fn wedge(ui: &mut egui::Ui, levels: &[u8]) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(w, 30.0), Sense::hover());
    let cw = w / levels.len() as f32;
    for (i, &l) in levels.iter().enumerate() {
        let r = Rect::from_min_size(pos2(rect.left() + i as f32 * cw, rect.top()), vec2(cw, 30.0));
        ui.painter().rect_filled(r.shrink(1.0), 0.0, Color32::from_gray(l));
    }
}

fn plot(ui: &mut egui::Ui, c: &Curve) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(w, 110.0), Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_stroke(rect, 2.0, Stroke::new(1.0, FAINT), egui::StrokeKind::Inside);
    let at = |x: f32, y: f32| pos2(rect.left() + x * rect.width(), rect.bottom() - y * rect.height());
    p.line_segment([at(0.0, 0.0), at(1.0, 1.0)], Stroke::new(1.0, FAINT));
    let grey = c.r == c.g && c.g == c.b;
    let cols = [
        Color32::from_rgb(220, 70, 70),
        Color32::from_rgb(70, 200, 90),
        Color32::from_rgb(80, 120, 240),
    ];
    for (ch, col) in cols.into_iter().enumerate().take(if grey { 1 } else { 3 }) {
        let pts: Vec<_> = (0..=64).map(|i| i as f32 / 64.0).map(|x| at(x, c.eval(x, ch))).collect();
        p.add(egui::Shape::line(pts, Stroke::new(1.5, if grey { TEXT } else { col })));
    }
}

/// (label, field accessor, min, max, decimals)
type Field = (&'static str, fn(&mut Curve) -> &mut f32, f32, f32, usize);
const GAMMA: Field = ("Gamma", |c| &mut c.gamma, 0.6, 1.8, 2);
const SHADOWS: Field = ("Shadows", |c| &mut c.shadows, 0.0, 1.0, 2);
const CONTRAST: Field = ("Contrast", |c| &mut c.contrast, -1.0, 1.0, 2);
const BLACK: Field = ("Black floor", |c| &mut c.black, 0.0, 0.15, 3);
const WHITE: Field = ("White point", |c| &mut c.white, 0.6, 1.0, 2);
const RED: Field = ("Red", |c| &mut c.r, 0.6, 1.0, 2);
const GREEN: Field = ("Green", |c| &mut c.g, 0.6, 1.0, 2);
const BLUE: Field = ("Blue", |c| &mut c.b, 0.6, 1.0, 2);

const STEPS: [(&str, &str); 7] = [
    (
        "Baseline",
        "In the monitor's own menu, turn off anything dynamic or \"enhanced\": DCR / dynamic contrast, ECO and game picture \
         modes, black stabiliser / shadow boost. Pick the standard picture mode and a 2.2 or default gamma. Set Contrast to the \
         factory default (usually 50; 75 on Dell and Samsung, 70 on LG): above that, highlights clip and shadows crush. \
         Apply baseline resets the GPU curve and saturation to neutral so every later step starts clean.",
    ),
    (
        "Brightness",
        "On an LCD, Brightness is just the backlight; it doesn't change contrast or colour. Set it so the white page is bright \
         but doesn't glare in your room. Dark room: roughly 20-40. Bright room: higher.",
    ),
    (
        "Highlights",
        "Every square must stay visible against the white. If the brightest ones vanish into the background, lower Contrast. \
         If squares near 254 still vanish at the factory contrast, pull White point down a touch.",
    ),
    (
        "Shadows",
        "The background must stay true black. Raise Shadows until you can see the squares from about 3-4 upward. \
         Only use Black floor if 1-2 are still invisible; it greys out blacks.",
    ),
    (
        "Gamma",
        "Lean back or squint until the stripes blur. Each solid square should melt into its stripes. Square darker than the \
         stripes: raise Gamma (left) or Shadows (right). Square brighter: lower it.",
    ),
    (
        "Colour",
        "The grey bar should be neutral with no tint; if it leans a colour, lower that gain. Saturation: the last 2-3 steps of each \
         ladder should stay distinct (if they merge, it's too saturated), and skin should look like skin. 50% = neutral.",
    ),
    (
        "Check",
        "Everything together. Go back to tweak anything. It's all saved already; turn on Keep applied to lock it in.",
    ),
];

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.tick(&ctx);

        if let Some(step) = self.cal {
            self.calibrate_ui(ui, step);
            return;
        }

        egui::Frame::new()
            .fill(BG)
            .inner_margin(18.0)
            .stroke(Stroke::new(1.0, FAINT))
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());

                // Title bar (custom, since the native one won't do pure black).
                ui.horizontal(|ui| {
                    ui.label(RichText::new("lumen").size(17.0).strong());
                    let drag = ui.interact(ui.available_rect_before_wrap(), ui.scope_id().with("drag"), Sense::click_and_drag());
                    if drag.drag_started() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(egui::Button::new(RichText::new("×").size(16.0).color(DIM)).frame(false))
                            .clicked()
                        {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        if ui.add(egui::Button::new(RichText::new("—").color(DIM)).frame(false)).clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                        }
                    });
                });

                if self.displays.is_empty() {
                    ui.label("No displays found.");
                    return;
                }

                let names: Vec<String> = self
                    .displays
                    .iter()
                    .map(|d| format!("{}  {}", d.name, d.gdi.trim_start_matches(r"\\.\")))
                    .collect();
                let before = self.sel;
                egui::ComboBox::from_id_salt("display")
                    .selected_text(&names[self.sel])
                    .width(ui.available_width())
                    .show_ui(ui, |ui| {
                        for (i, n) in names.iter().enumerate() {
                            ui.selectable_value(&mut self.sel, i, n);
                        }
                    });
                if before != self.sel {
                    self.persist();
                    self.push_curve();
                }

                let cal = egui::Button::new(RichText::new("Calibrate with test patterns").color(Color32::BLACK).strong())
                    .fill(Color32::from_gray(235));
                if ui.add_sized([ui.available_width(), 30.0], cal).clicked() {
                    self.cal = Some(0);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(true));
                }

                heading(ui, "SHADOWS  ·  every step should be visible");
                wedge(ui, &[0, 3, 6, 9, 12, 16, 20, 25, 30, 36, 42, 50]);
                heading(ui, "HIGHLIGHTS");
                wedge(ui, &[200, 210, 218, 225, 231, 236, 240, 244, 248, 251, 253, 255]);

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .max_height(ui.available_height() - 64.0)
                    .show(ui, |ui| {
                        self.hardware_ui(ui);
                        self.curve_ui(ui);
                        self.advanced_ui(ui);
                    });

                ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                    let st = if !self.status.is_empty() {
                        self.status.clone()
                    } else if self.daemon {
                        "Enforcing in background · survives fullscreen, sleep, driver resets".into()
                    } else {
                        "Not enforcing · turn on Keep applied".into()
                    };
                    ui.label(RichText::new(st).size(11.0).color(DIM));
                    ui.horizontal(|ui| {
                        let mut on = self.autostart;
                        let label = if on {
                            RichText::new("Keep applied · on").color(Color32::BLACK)
                        } else {
                            RichText::new("Keep applied")
                        };
                        if ui.toggle_value(&mut on, label).changed() {
                            self.persist();
                            if on {
                                match sys::enable_autostart() {
                                    Ok(()) => self.status.clear(),
                                    Err(e) => self.status = e,
                                }
                            } else {
                                sys::disable_autostart();
                            }
                            self.autostart = sys::autostart_enabled();
                            std::thread::sleep(Duration::from_millis(300));
                            self.daemon = sys::daemon_running();
                            sys::post_daemon(sys::MSG_PAUSE);
                        }
                    });
                });
            });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.persist();
        drop(self.jobs.take());
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
        sys::post_daemon(sys::MSG_RELOAD);
    }
}

impl App {
    /// One DDC/CI slider bound to a VCP code.
    fn hw_row(&mut self, ui: &mut egui::Ui, code: u8) {
        let gdi = self.displays[self.sel].gdi.clone();
        let live = self.probes.get(&gdi).and_then(|p| p.vals.get(&code).copied());
        let hw = &self.profile().unwrap().hardware;
        let (label, want) = match code {
            vcp::BRIGHTNESS => ("Brightness", hw.brightness),
            vcp::CONTRAST => ("Contrast", hw.contrast),
            vcp::RED_GAIN => ("Red gain", hw.red),
            vcp::GREEN_GAIN => ("Green gain", hw.green),
            _ => ("Blue gain", hw.blue),
        };
        let max = live.map(|(_, m)| m.max(1)).unwrap_or(100);
        let mut v = want.or(live.map(|(c, _)| c)).unwrap_or(0) as f32;
        if row(ui, label, &mut v, 0.0..=max as f32, live.is_some(), |v| format!("{v:.0}")) {
            let v = v.round() as u32;
            let hw = &mut self.profile().unwrap().hardware;
            *match code {
                vcp::BRIGHTNESS => &mut hw.brightness,
                vcp::CONTRAST => &mut hw.contrast,
                vcp::RED_GAIN => &mut hw.red,
                vcp::GREEN_GAIN => &mut hw.green,
                _ => &mut hw.blue,
            } = Some(v);
            self.push_vcp(code, v);
        }
    }

    fn curve_rows(&mut self, ui: &mut egui::Ui, fields: &[Field]) {
        let mut c = self.profile().unwrap().curve.clone();
        let before = c.clone();
        for &(label, field, lo, hi, dec) in fields {
            let on = c.enabled;
            row(ui, label, field(&mut c), lo..=hi, on, |v| format!("{v:.dec$}"));
        }
        if c != before {
            self.profile().unwrap().curve = c;
            self.push_curve();
        }
    }

    fn vibrance_row(&mut self, ui: &mut egui::Ui) {
        let gdi = self.displays[self.sel].gdi.clone();
        let live = self.vib.get(&gdi).copied();
        let max = live.map(|(_, m)| m.max(1)).unwrap_or(63);
        let mut v = self.profile().unwrap().vibrance.or(live.map(|(c, _)| c)).unwrap_or(0) as f32;
        let r = row(ui, "Saturation", &mut v, 0.0..=max as f32, live.is_some(), |v| {
            format!("{:.0}%", 50.0 + v * 50.0 / max as f32)
        });
        if r {
            self.push_vibrance(v.round() as u32);
        }
    }

    fn hardware_ui(&mut self, ui: &mut egui::Ui) {
        let gdi = self.displays[self.sel].gdi.clone();
        let state = match self.probes.get(&gdi) {
            None => "reading monitor…",
            Some(p) if p.vals.is_empty() => "DDC/CI unavailable · enable it in the monitor OSD",
            Some(_) => "",
        };
        heading(ui, "MONITOR  ·  DDC/CI hardware settings");
        if !state.is_empty() {
            ui.label(RichText::new(state).size(11.5).color(DIM));
        }
        for code in [vcp::BRIGHTNESS, vcp::CONTRAST, vcp::RED_GAIN, vcp::GREEN_GAIN, vcp::BLUE_GAIN] {
            self.hw_row(ui, code);
        }
        if ui
            .small_button("Stop managing hardware")
            .on_hover_text("Forget saved DDC values; the monitor keeps its current state")
            .clicked()
        {
            self.profile().unwrap().hardware = Default::default();
            self.changed_at = Some(Instant::now());
        }
    }

    fn curve_ui(&mut self, ui: &mut egui::Ui) {
        heading(ui, "SIGNAL  ·  GPU lookup table + saturation");
        let mut c = self.profile().unwrap().curve.clone();
        let before = c.clone();
        ui.horizontal(|ui| {
            ui.checkbox(&mut c.enabled, "Curve enabled");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Neutral").clicked() {
                    c = Curve::default();
                }
                if ui
                    .small_button("Open shadows")
                    .on_hover_text("Starting point for a dark, crushed IPS panel")
                    .clicked()
                {
                    c = Curve {
                        gamma: 1.12,
                        shadows: 0.35,
                        contrast: -0.15,
                        ..Curve::default()
                    };
                }
            });
        });
        plot(ui, &c);
        if c != before {
            self.profile().unwrap().curve = c.clone();
            self.push_curve();
        }
        self.curve_rows(ui, &[GAMMA, SHADOWS, CONTRAST, BLACK, WHITE, RED, GREEN, BLUE]);
        self.vibrance_row(ui);

        if !self.ramp_ok && !c.is_identity() {
            ui.add_space(4.0);
            egui::Frame::new()
                .stroke(Stroke::new(1.0, Color32::from_gray(70)))
                .inner_margin(8.0)
                .corner_radius(3)
                .show(ui, |ui| {
                    ui.label(RichText::new("Windows is refusing this curve (it clamps strong gamma ramps by default).").size(11.5));
                    if !self.unlocked {
                        if ui.button("Unlock full range (admin, one time)").clicked() && !sys::request_gamma_unlock() {
                            self.status = "unlock cancelled".into();
                        }
                    } else {
                        ui.label(
                            RichText::new("Unlocked · sign out and back in for it to take effect.")
                                .size(11.5)
                                .color(DIM),
                        );
                    }
                });
        }
    }

    fn advanced_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        egui::CollapsingHeader::new(
            RichText::new("ADVANCED  ·  raw VCP")
                .size(10.5)
                .color(DIM)
                .extra_letter_spacing(1.6),
        )
        .show(ui, |ui| {
            ui.label(
                RichText::new(
                    "Send any MCCS code (hex). Writes are remembered and re-applied. e.g. DC=0 standard mode, 14=0B user colour.",
                )
                .size(11.0)
                .color(DIM),
            );
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.vcp_code).desired_width(40.0).hint_text("code"));
                ui.add(egui::TextEdit::singleline(&mut self.vcp_val).desired_width(60.0).hint_text("value"));
                let parsed = (
                    u8::from_str_radix(self.vcp_code.trim().trim_start_matches("0x"), 16),
                    self.vcp_val
                        .trim()
                        .parse::<u32>()
                        .or_else(|_| u32::from_str_radix(self.vcp_val.trim().trim_start_matches("0x"), 16)),
                );
                if ui.button("Read").clicked()
                    && let Ok(code) = parsed.0
                {
                    let gdi = self.displays[self.sel].gdi.clone();
                    self.status = match Ddc::open(&gdi).and_then(|d| d.get(code)) {
                        Some((c, m)) => format!("VCP {code:02X} = {c} (max {m})"),
                        None => format!("VCP {code:02X}: no reply"),
                    };
                }
                if ui.button("Write").clicked()
                    && let (Ok(code), Ok(v)) = parsed
                {
                    let hw = &mut self.profile().unwrap().hardware;
                    hw.extra.retain(|(c, _)| *c != code);
                    hw.extra.push((code, v));
                    self.push_vcp(code, v);
                    self.status = format!("VCP {code:02X} ← {v}");
                }
            });
            let extra = self.profile().unwrap().hardware.extra.clone();
            for (c, v) in extra {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{c:02X} = {v}")).monospace().size(12.0));
                    if ui.small_button("forget").clicked() {
                        self.profile().unwrap().hardware.extra.retain(|(x, _)| *x != c);
                        self.changed_at = Some(Instant::now());
                    }
                });
            }
        });
    }

    /// Neutral curve and saturation: a known-good starting point.
    fn baseline(&mut self) {
        let has_vib = self.display().is_some_and(|d| self.vib.contains_key(&d.gdi));
        self.profile().unwrap().curve = Curve::default();
        self.push_curve();
        if has_vib {
            self.push_vibrance(0);
        }
    }

    fn calibrate_ui(&mut self, ui: &mut egui::Ui, step: usize) {
        let ctx = ui.ctx().clone();
        let full = ui.max_rect();
        let panel_h = 230.0;
        let area = Rect::from_min_max(full.min, pos2(full.right(), full.bottom() - panel_h));
        let ppp = ctx.pixels_per_point();
        {
            let p = ui.painter_at(area);
            match step {
                0 => {
                    p.rect_filled(area, 0.0, Color32::from_gray(118));
                }
                1 => patterns::brightness(&p, area),
                2 => patterns::white_level(&p, area),
                3 => patterns::black_level(&p, area),
                4 => patterns::gamma(&p, area, &self.tex, ppp),
                5 => patterns::colour(&p, area),
                _ => patterns::overview(&p, area, &self.tex, ppp),
            }
        }

        let panel = Rect::from_min_max(pos2(full.left(), area.bottom()), full.max);
        ui.painter().rect_filled(panel, 0.0, BG);
        ui.painter()
            .line_segment([panel.left_top(), panel.right_top()], Stroke::new(1.0, FAINT));
        let w = 620.0f32.min(panel.width() - 48.0);
        let inner = Rect::from_min_size(pos2(panel.center().x - w / 2.0, panel.top() + 16.0), vec2(w, panel_h - 28.0));

        let mut exit = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        ui.scope_builder(egui::UiBuilder::new().max_rect(inner), |ui| {
            let (title, hint) = STEPS[step];
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{}/{}", step + 1, STEPS.len())).monospace().color(DIM));
                ui.label(RichText::new(title).size(16.0).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(egui::Button::new(RichText::new("×  Esc").color(DIM)).frame(false)).clicked() {
                        exit = true;
                    }
                });
            });
            ui.label(RichText::new(hint).size(12.5).color(Color32::from_gray(170)));
            ui.add_space(4.0);

            match step {
                0 => {
                    self.hw_row(ui, vcp::CONTRAST);
                    if ui.button("Apply baseline").clicked() {
                        self.baseline();
                    }
                }
                1 => self.hw_row(ui, vcp::BRIGHTNESS),
                2 => {
                    self.hw_row(ui, vcp::CONTRAST);
                    self.curve_rows(ui, &[WHITE]);
                }
                3 => self.curve_rows(ui, &[SHADOWS, BLACK]),
                4 => self.curve_rows(ui, &[GAMMA, SHADOWS, CONTRAST]),
                5 => {
                    self.vibrance_row(ui);
                    for code in [vcp::RED_GAIN, vcp::GREEN_GAIN, vcp::BLUE_GAIN] {
                        self.hw_row(ui, code);
                    }
                }
                _ => {}
            }

            ui.with_layout(egui::Layout::bottom_up(egui::Align::Max), |ui| {
                ui.horizontal(|ui| {
                    let last = step + 1 == STEPS.len();
                    let next = egui::Button::new(RichText::new(if last { "Finish" } else { "Next" }).color(Color32::BLACK).strong())
                        .fill(Color32::from_gray(235));
                    if ui.add(next).clicked() {
                        if last {
                            exit = true;
                        } else {
                            self.cal = Some(step + 1);
                        }
                    }
                    if step > 0 && ui.button("Back").clicked() {
                        self.cal = Some(step - 1);
                    }
                });
            });
        });

        if exit {
            self.cal = None;
            self.persist();
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
        }
    }
}
