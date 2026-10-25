// Shared by build.rs (exe icon) and the app (window/taskbar icon); no dependencies.

/// RGBA pixels of the lumen mark at `n`x`n`: a half-lit disc in a ring, on a
/// black rounded tile. Supersampled 4x4 for clean edges at small sizes.
pub fn icon_rgba(n: u32) -> Vec<u8> {
    const SS: u32 = 4;
    let mut out = Vec::with_capacity((n * n * 4) as usize);
    for py in 0..n {
        for px in 0..n {
            let (mut lum, mut cov) = (0.0f32, 0.0f32);
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = (px as f32 + (sx as f32 + 0.5) / SS as f32) / n as f32;
                    let y = (py as f32 + (sy as f32 + 0.5) / SS as f32) / n as f32;
                    if let Some(l) = sample(x, y) {
                        lum += l;
                        cov += 1.0;
                    }
                }
            }
            let a = cov / (SS * SS) as f32;
            let g = if cov > 0.0 { (lum / cov * 255.0).round() as u8 } else { 0 };
            out.extend_from_slice(&[g, g, g, (a * 255.0).round() as u8]);
        }
    }
    out
}

/// Luminance at a point in the unit square, or `None` outside the tile.
fn sample(x: f32, y: f32) -> Option<f32> {
    let (m, r) = (0.03, 0.22);
    let qx = (x - 0.5).abs() - (0.5 - m - r);
    let qy = (y - 0.5).abs() - (0.5 - m - r);
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r;
    if outside > 0.0 {
        return None;
    }
    let d = ((x - 0.5).powi(2) + (y - 0.5).powi(2)).sqrt();
    Some(match d {
        d if d < 0.27 => {
            if x < 0.5 {
                1.0
            } else {
                0.0
            }
        }
        d if d < 0.31 => 0.0,
        d if d < 0.345 => 1.0,
        _ => 0.0,
    })
}
