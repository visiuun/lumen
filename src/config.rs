use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::display::Ramp;

/// Values written straight into the monitor over DDC/CI. `None` = leave alone.
#[derive(Serialize, Deserialize, Clone, PartialEq, Default, Debug)]
#[serde(default)]
pub struct Hardware {
    pub brightness: Option<u32>,
    pub contrast: Option<u32>,
    pub red: Option<u32>,
    pub green: Option<u32>,
    pub blue: Option<u32>,
    /// Arbitrary extra VCP writes, e.g. vendor codes: [[0xDC, 0]].
    pub extra: Vec<(u8, u32)>,
}

impl Hardware {
    pub fn gains_set(&self) -> bool {
        self.red.is_some() || self.green.is_some() || self.blue.is_some()
    }
}

/// Transfer curve loaded into the GPU's hardware LUT.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Curve {
    pub enabled: bool,
    /// > 1 brightens midtones (output = input^(1/gamma)).
    pub gamma: f32,
    /// 0..1, lifts crushed near-blacks without greying the black point.
    pub shadows: f32,
    /// -1..1, negative flattens an over-contrasty panel, positive adds punch.
    pub contrast: f32,
    /// Output floor / ceiling (0..1).
    pub black: f32,
    pub white: f32,
    pub r: f32,
    pub g: f32,
    pub b: f32,
}

impl Default for Curve {
    fn default() -> Self {
        Curve {
            enabled: true,
            gamma: 1.0,
            shadows: 0.0,
            contrast: 0.0,
            black: 0.0,
            white: 1.0,
            r: 1.0,
            g: 1.0,
            b: 1.0,
        }
    }
}

impl Curve {
    pub fn is_identity(&self) -> bool {
        let d = Curve::default();
        Curve {
            enabled: true,
            ..self.clone()
        } == d
    }

    pub fn eval(&self, x: f32, ch: usize) -> f32 {
        let mut y = x;
        // Contrast: blend toward smoothstep (S-curve) or its inverse (flatter).
        if self.contrast > 0.0 {
            let s = y * y * (3.0 - 2.0 * y);
            y += (s - y) * self.contrast;
        } else if self.contrast < 0.0 {
            let s = 0.5 - ((1.0 - 2.0 * y).clamp(-1.0, 1.0).asin() / 3.0).sin();
            y += (s - y) * -self.contrast;
        }
        y = y.max(0.0).powf(1.0 / self.gamma.max(0.05));
        // Toe lift peaking around 20% input; stays monotonic for shadows <= 1.
        y += self.shadows * 2.5 * y * (1.0 - y).powi(4);
        y = self.black + (self.white - self.black) * y;
        y *= [self.r, self.g, self.b][ch];
        y.clamp(0.0, 1.0)
    }

    pub fn ramp(&self) -> Ramp {
        let mut r = [[0u16; 256]; 3];
        for (ch, lut) in r.iter_mut().enumerate() {
            let mut prev = 0u16;
            for (i, v) in lut.iter_mut().enumerate() {
                // Windows rejects non-monotonic ramps.
                let o = ((self.eval(i as f32 / 255.0, ch) * 65535.0).round() as u16).max(prev);
                *v = o;
                prev = o;
            }
        }
        r
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Default, Debug)]
#[serde(default)]
pub struct Profile {
    pub name: String,
    pub hardware: Hardware,
    pub curve: Curve,
    /// NVIDIA Digital Vibrance level (0 = driver default). `None` = leave alone.
    pub vibrance: Option<u32>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Config {
    /// Re-assert DDC values if the monitor drifts (input switch, OSD, power cycle).
    pub enforce_hardware: bool,
    /// Keyed by stable monitor device path.
    pub profiles: BTreeMap<String, Profile>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enforce_hardware: true,
            profiles: BTreeMap::new(),
        }
    }
}

pub fn dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| ".".into());
    base.join("lumen")
}

pub fn path() -> PathBuf {
    dir().join("config.json")
}

pub fn mtime() -> Option<SystemTime> {
    std::fs::metadata(path()).and_then(|m| m.modified()).ok()
}

impl Config {
    pub fn load() -> Config {
        std::fs::read_to_string(path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(dir())?;
        let tmp = dir().join("config.json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(tmp, path())
    }
}
