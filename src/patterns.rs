//! Calibration test patterns, drawn procedurally. Every value is an exact 8-bit
//! level, so what you see is the panel + LUT + vibrance and nothing else.

use eframe::egui::{self, Color32, FontId, Painter, Rect, TextureHandle, pos2, vec2};

pub struct Textures {
    /// 1x2 texture (black line, white line), tiled 1:1 onto physical pixels.
    stripes: TextureHandle,
    /// 1x2 texture (black line, level-64 line) for the low-light gamma check.
    stripes_dark: TextureHandle,
}

impl Textures {
    pub fn new(ctx: &egui::Context) -> Self {
        let tex = |name, a, b| {
            ctx.load_texture(
                name,
                egui::ColorImage::new([1, 2], vec![Color32::from_gray(a), Color32::from_gray(b)]),
                egui::TextureOptions::NEAREST_REPEAT,
            )
        };
        Textures {
            stripes: tex("stripes", 0, 255),
            stripes_dark: tex("stripes_dark", 0, 64),
        }
    }
}

/// Snap a rect onto the physical pixel grid so 1-px stripes stay crisp.
fn snap(r: Rect, ppp: f32) -> Rect {
    let s = |v: f32| (v * ppp).round() / ppp;
    Rect::from_min_max(pos2(s(r.left()), s(r.top())), pos2(s(r.right()), s(r.bottom())))
}

fn label(p: &Painter, at: egui::Pos2, text: &str, color: Color32) {
    p.text(at, egui::Align2::CENTER_TOP, text, FontId::proportional(13.0), color);
}

/// A row of square patches at the given levels on a background, numbered.
fn patches(p: &Painter, area: Rect, bg: u8, levels: &[u8], label_color: Color32) {
    p.rect_filled(area, 0.0, Color32::from_gray(bg));
    let n = levels.len() as f32;
    let size = (area.width() / (n * 1.35)).min(area.height() * 0.45);
    let gap = (area.width() - size * n) / (n + 1.0);
    for (i, &l) in levels.iter().enumerate() {
        let x = area.left() + gap + i as f32 * (size + gap);
        let r = Rect::from_min_size(pos2(x, area.center().y - size / 2.0), vec2(size, size));
        p.rect_filled(r, 0.0, Color32::from_gray(l));
        label(p, pos2(r.center().x, r.bottom() + 10.0), &l.to_string(), label_color);
    }
}

pub fn black_level(p: &Painter, area: Rect) {
    patches(p, area, 0, &[1, 2, 3, 4, 5, 6, 8, 10, 13, 16, 20, 25], Color32::from_gray(70));
}

pub fn white_level(p: &Painter, area: Rect) {
    patches(
        p,
        area,
        255,
        &[230, 234, 238, 241, 244, 246, 248, 250, 252, 253, 254],
        Color32::from_gray(150),
    );
}

pub fn brightness(p: &Painter, area: Rect) {
    // Paper-white page on mid grey, with body-text-like bars: judge glare/comfort.
    p.rect_filled(area, 0.0, Color32::from_gray(118));
    let page = Rect::from_center_size(area.center(), vec2(area.width() * 0.42, area.height() * 0.8));
    p.rect_filled(page, 0.0, Color32::WHITE);
    let mut y = page.top() + 40.0;
    while y < page.bottom() - 40.0 {
        let w = page.width() - 80.0 - ((y * 7.3) % 120.0);
        p.rect_filled(
            Rect::from_min_size(pos2(page.left() + 40.0, y), vec2(w, 7.0)),
            1.0,
            Color32::from_gray(30),
        );
        y += 20.0;
    }
}

/// Classic stripe-vs-solid gamma check. The solid square matches the 50%
/// stripe average only when the end-to-end response is gamma 2.2.
pub fn gamma(p: &Painter, area: Rect, tex: &Textures, ppp: f32) {
    p.rect_filled(area, 0.0, Color32::from_gray(0));
    let half = area.width() / 2.0;
    for (i, (t, solid, title)) in [(&tex.stripes, 186u8, "overall gamma"), (&tex.stripes_dark, 47u8, "shadow gamma")]
        .into_iter()
        .enumerate()
    {
        let cell = Rect::from_min_size(pos2(area.left() + i as f32 * half, area.top()), vec2(half, area.height()));
        let field = snap(Rect::from_center_size(cell.center(), vec2(half * 0.75, area.height() * 0.7)), ppp);
        let px = field.size() * ppp;
        p.image(
            t.id(),
            field,
            Rect::from_min_max(pos2(0.0, 0.0), pos2(px.x, px.y / 2.0)),
            Color32::WHITE,
        );
        let sq = Rect::from_center_size(field.center(), vec2(field.width() * 0.4, field.height() * 0.4));
        p.rect_filled(sq, 0.0, Color32::from_gray(solid));
        label(p, pos2(field.center().x, field.bottom() + 12.0), title, Color32::from_gray(110));
    }
}

const HUES: [(u8, u8, u8); 6] = [(255, 0, 0), (255, 255, 0), (0, 255, 0), (0, 255, 255), (0, 0, 255), (255, 0, 255)];

fn mix(a: (u8, u8, u8), grey: u8, t: f32) -> Color32 {
    let m = |c: u8| (grey as f32 + (c as f32 - grey as f32) * t).round() as u8;
    Color32::from_rgb(m(a.0), m(a.1), m(a.2))
}

pub fn colour(p: &Painter, area: Rect) {
    p.rect_filled(area, 0.0, Color32::from_gray(24));
    let pad = 40.0;
    let inner = area.shrink(pad);
    let row_h = inner.height() / 10.0;

    // Neutral grey ramp: any tint = white balance off.
    let ramp = Rect::from_min_size(inner.min, vec2(inner.width(), row_h * 1.1));
    let steps = 32;
    for i in 0..steps {
        let w = ramp.width() / steps as f32;
        let r = Rect::from_min_size(pos2(ramp.left() + i as f32 * w, ramp.top()), vec2(w + 0.5, ramp.height()));
        p.rect_filled(r, 0.0, Color32::from_gray((i * 255 / (steps - 1)) as u8));
    }
    p.rect_stroke(
        ramp.expand(3.0),
        0.0,
        egui::Stroke::new(1.0, Color32::from_gray(90)),
        egui::StrokeKind::Outside,
    );

    // Saturation ladders: grey -> full hue in 10 steps. Last steps must stay distinct.
    for (j, &h) in HUES.iter().enumerate() {
        let y = ramp.bottom() + row_h * 0.4 + j as f32 * row_h;
        for i in 0..10 {
            let w = inner.width() * 0.62 / 10.0;
            let r = Rect::from_min_size(pos2(inner.left() + i as f32 * w, y), vec2(w - 3.0, row_h - 6.0));
            p.rect_filled(r, 0.0, mix(h, 128, (i + 1) as f32 / 10.0));
        }
    }

    // Skin tones (light -> deep). Should look like people, not orange or pink.
    let skin = [
        (255, 224, 196),
        (241, 194, 167),
        (224, 172, 105),
        (198, 134, 66),
        (141, 85, 36),
        (92, 51, 23),
    ];
    let sx = inner.left() + inner.width() * 0.66;
    let sw = inner.width() * 0.34;
    for (j, &(r, g, b)) in skin.iter().enumerate() {
        let y = ramp.bottom() + row_h * 0.4 + j as f32 * row_h;
        p.rect_filled(
            Rect::from_min_size(pos2(sx, y), vec2(sw, row_h - 6.0)),
            2.0,
            Color32::from_rgb(r, g, b),
        );
    }

    // Smooth colour gradients: banding / posterisation check.
    let gy = ramp.bottom() + row_h * 0.6 + 6.0 * row_h;
    let gh = (inner.bottom() - gy) / 3.0;
    let cols = [(255, 0, 0), (0, 255, 0), (0, 0, 255)];
    for (k, c) in cols.into_iter().enumerate() {
        let n = 128;
        for i in 0..n {
            let w = inner.width() / n as f32;
            let t = i as f32 / (n - 1) as f32;
            let r = Rect::from_min_size(pos2(inner.left() + i as f32 * w, gy + k as f32 * gh), vec2(w + 0.5, gh - 4.0));
            p.rect_filled(
                r,
                0.0,
                Color32::from_rgb((c.0 as f32 * t) as u8, (c.1 as f32 * t) as u8, (c.2 as f32 * t) as u8),
            );
        }
    }
}

/// Everything at once: final sanity check.
pub fn overview(p: &Painter, area: Rect, tex: &Textures, ppp: f32) {
    let h = area.height() / 3.0;
    let row = |i: f32| Rect::from_min_size(pos2(area.left(), area.top() + i * h), vec2(area.width(), h));
    black_level(p, row(0.0));
    gamma(p, row(1.0), tex, ppp);
    white_level(p, row(2.0));
}
