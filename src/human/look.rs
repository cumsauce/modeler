//! The human's built-in skin look and the colour inputs of its hair,
//! brows and lashes.
//!
//! The skin is a card-domain material over the template uvs: a `tone`
//! (a colour, or 0..1 along a melanin ramp) mottled by noise, lips /
//! nails / areola tinted by their `zone`s, a faint redness on the cheeks,
//! nose, ears, knuckles, knees and elbows, an oilier T-zone and rougher
//! elbows and knees, and a little diffuse transmission. Any `material`
//! item on the body replaces it.

use serde::{Deserialize, Serialize};
use serde_json::json;

/// A colour field: `[r, g, b]` / `[r, g, b, a]` (0..1 or 0..255) or
/// `"#rrggbb"`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(untagged)]
pub enum ColorIn {
    Rgb(Vec<f32>),
    Hex(String),
}

impl ColorIn {
    /// sRGB 0..1.
    pub fn rgb(&self) -> Result<[f32; 3], String> {
        match self {
            ColorIn::Rgb(v) if v.len() == 3 || v.len() == 4 => {
                let k = if v.iter().take(3).any(|x| *x > 1.0) {
                    1.0 / 255.0
                } else {
                    1.0
                };
                Ok([v[0] * k, v[1] * k, v[2] * k].map(|x| x.clamp(0.0, 1.0)))
            }
            ColorIn::Rgb(v) => Err(format!("a colour is [r, g, b], got {} values", v.len())),
            ColorIn::Hex(s) => hex_rgb(s),
        }
    }
}

fn hex_rgb(s: &str) -> Result<[f32; 3], String> {
    let h = s.trim_start_matches('#');
    let digits: Vec<u32> = h.chars().filter_map(|c| c.to_digit(16)).collect();
    if digits.len() != h.len() {
        return Err(format!("`{s}` is not a #rrggbb colour"));
    }
    let v = match digits.len() {
        3 | 4 => [digits[0] * 17, digits[1] * 17, digits[2] * 17],
        6 | 8 => [
            digits[0] * 16 + digits[1],
            digits[2] * 16 + digits[3],
            digits[4] * 16 + digits[5],
        ],
        _ => return Err(format!("`{s}` is not a #rrggbb colour")),
    };
    Ok(v.map(|x| x as f32 / 255.0))
}

/// `skin_tone`: 0..1 from light to dark along a melanin ramp, or a colour.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(untagged)]
pub enum SkinTone {
    Level(f32),
    Color(ColorIn),
}

/// The melanin ramp (sRGB): very light .. very dark.
const RAMP: [[f32; 3]; 5] = [
    [0.96, 0.80, 0.70],
    [0.87, 0.67, 0.55],
    [0.72, 0.51, 0.38],
    [0.49, 0.33, 0.23],
    [0.28, 0.18, 0.12],
];

/// The default tone (between ramp 0.2 and 0.3).
pub const DEFAULT_TONE: [f32; 3] = [0.80, 0.62, 0.52];

impl SkinTone {
    pub fn rgb(&self) -> Result<[f32; 3], String> {
        match self {
            SkinTone::Level(t) => {
                if !t.is_finite() || !(0.0..=1.0).contains(t) {
                    return Err(format!(
                        "skin_tone: 0 (light) .. 1 (dark) or a colour, got {t}"
                    ));
                }
                let f = t * 4.0;
                let i = (f.floor() as usize).min(3);
                let k = f - i as f32;
                Ok([0, 1, 2].map(|c| RAMP[i][c] + (RAMP[i + 1][c] - RAMP[i][c]) * k))
            }
            SkinTone::Color(c) => c.rgb().map_err(|e| format!("skin_tone: {e}")),
        }
    }
}

pub fn hex(c: [f32; 3]) -> String {
    let b = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(c[0]), b(c[1]), b(c[2]))
}

fn mul(c: [f32; 3], k: [f32; 3]) -> [f32; 3] {
    [c[0] * k[0], c[1] * k[1], c[2] * k[2]]
}

/// The built-in skin graph for `tone` (sRGB).
pub fn skin_graph(tone: [f32; 3]) -> crate::material::MatGraph {
    // Darker skin shows less redness and darker, browner lips.
    let lum = 0.3 * tone[0] + 0.59 * tone[1] + 0.11 * tone[2];
    let flush = mul(tone, [1.02, 0.86, 0.84]);
    let red = mul(tone, [1.04, 0.70, 0.68]);
    let lips = if lum > 0.45 {
        mul(tone, [0.86, 0.56, 0.58])
    } else {
        mul(tone, [0.72, 0.52, 0.52])
    };
    let areola = mul(tone, [0.72, 0.52, 0.48]);
    let nails = [
        (tone[0] * 0.55 + 0.42).min(1.0),
        (tone[1] * 0.55 + 0.32).min(1.0),
        (tone[2] * 0.55 + 0.32).min(1.0),
    ];
    let red_k = (0.45 * (lum / 0.6)).clamp(0.12, 0.45);
    crate::material::MatGraph {
        domain: "card".into(),
        tile: 1.0,
        resolution: 1024,
        nodes: vec![
            json!({"type": "noise", "name": "pores", "size": 0.004, "octaves": 2, "seed": 11}),
            json!({"type": "noise", "name": "mottle", "size": 0.06, "octaves": 4, "seed": 4}),
            json!({"type": "ramp", "name": "base", "input": "mottle",
                   "stops": [[0.0, hex(mul(tone, [0.93; 3]))], [0.5, hex(tone)], [1.0, hex(flush)]]}),
            json!({"type": "zone", "name": "z_cheeks", "zone": "cheeks", "soft": 0.01}),
            json!({"type": "zone", "name": "z_nose", "zone": "nose", "soft": 0.006}),
            json!({"type": "zone", "name": "z_ears", "zone": "ears", "soft": 0.004}),
            json!({"type": "zone", "name": "z_knuckles", "zone": "knuckles", "soft": 0.004}),
            json!({"type": "zone", "name": "z_knees", "zone": "knees", "soft": 0.01}),
            json!({"type": "zone", "name": "z_elbows", "zone": "elbows", "soft": 0.01}),
            json!({"type": "zone", "name": "z_lips", "zone": "lips", "soft": 0.001}),
            json!({"type": "zone", "name": "z_nails", "zone": "nails"}),
            json!({"type": "zone", "name": "z_areola", "zone": "areola", "soft": 0.002}),
            json!({"type": "zone", "name": "z_tzone", "zone": "tzone", "soft": 0.01}),
            json!({"type": "math", "name": "r1", "op": "max", "a": "z_cheeks", "b": "z_nose"}),
            json!({"type": "math", "name": "r2", "op": "max", "a": "r1", "b": "z_ears"}),
            json!({"type": "math", "name": "r3", "op": "max", "a": "r2", "b": "z_knuckles"}),
            json!({"type": "math", "name": "r4", "op": "max", "a": "z_knees", "b": "z_elbows"}),
            json!({"type": "math", "name": "r5", "op": "max", "a": "r3", "b": "r4"}),
            json!({"type": "math", "name": "redness", "op": "mul", "a": "r5", "b": red_k}),
            json!({"type": "blend", "name": "c0", "a": "base", "b": hex(mul(tone, [0.88; 3])), "opacity": 0.12}),
            json!({"type": "blend", "name": "c1", "a": "c0", "b": hex(red), "opacity": "redness"}),
            json!({"type": "math", "name": "lip_k", "op": "mul", "a": "z_lips", "b": 0.85}),
            json!({"type": "blend", "name": "c2", "a": "c1", "b": hex(lips), "opacity": "lip_k"}),
            json!({"type": "math", "name": "are_k", "op": "mul", "a": "z_areola", "b": 0.75}),
            json!({"type": "blend", "name": "c3", "a": "c2", "b": hex(areola), "opacity": "are_k"}),
            json!({"type": "blend", "name": "col", "a": "c3", "b": hex(nails), "opacity": "z_nails"}),
            // Roughness: pores, an oily T-zone, rough knees and elbows,
            // smoother lips, glossy nails.
            json!({"type": "remap", "name": "rough0", "input": "pores", "from": [0.0, 1.0], "to": [0.48, 0.64]}),
            json!({"type": "math", "name": "oil", "op": "mul", "a": "z_tzone", "b": -0.14}),
            json!({"type": "math", "name": "rough1", "op": "add", "a": "rough0", "b": "oil"}),
            json!({"type": "math", "name": "dry", "op": "mul", "a": "r4", "b": 0.14}),
            json!({"type": "math", "name": "rough2", "op": "add", "a": "rough1", "b": "dry"}),
            json!({"type": "math", "name": "rough3", "op": "mix", "a": "rough2", "b": 0.38, "c": "z_lips"}),
            json!({"type": "math", "name": "rough", "op": "mix", "a": "rough3", "b": 0.22, "c": "z_nails"}),
            json!({"type": "math", "name": "h", "op": "mul", "a": "pores", "b": 0.00015}),
            json!({"type": "output", "name": "skin", "base_color": "col", "roughness": "rough",
                   "height": "h", "transmission": 0.3}),
        ],
        params: Default::default(),
    }
}

/// The skin graph for a body item (its `skin_tone`, else the default).
pub fn skin_for(def: &crate::body::BodyDef) -> Result<crate::material::MatGraph, String> {
    let tone = match &def.skin_tone {
        Some(t) => t.rgb()?,
        None => DEFAULT_TONE,
    };
    Ok(skin_graph(tone))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tones_parse_and_ramp() {
        let l = SkinTone::Level(0.0).rgb().unwrap_or_else(|e| panic!("{e}"));
        let d = SkinTone::Level(1.0).rgb().unwrap_or_else(|e| panic!("{e}"));
        assert!(l[0] > d[0] + 0.5);
        assert!(SkinTone::Level(1.5).rgb().is_err());
        let c = SkinTone::Color(ColorIn::Hex("#8d5524".into()))
            .rgb()
            .unwrap_or_else(|e| panic!("{e}"));
        assert!((c[0] - 141.0 / 255.0).abs() < 1e-4);
        let c = ColorIn::Rgb(vec![255.0, 0.0, 0.0])
            .rgb()
            .unwrap_or_else(|e| panic!("{e}"));
        assert!((c[0] - 1.0).abs() < 1e-6);
        // Every node parses.
        let g = skin_graph(DEFAULT_TONE);
        for n in &g.nodes {
            let ty = n["type"].as_str().unwrap_or("");
            let mut b = n.clone();
            if let Some(m) = b.as_object_mut() {
                m.remove("type");
                m.remove("name");
            }
            crate::material::node::parse_body(ty, &b, "t").unwrap_or_else(|e| panic!("{e}"));
        }
    }
}

#[cfg(test)]
mod bake_check {
    #[test]
    fn skin_bakes_to_skin_colours() {
        if !crate::human::asset::available() {
            return;
        }
        let g = super::skin_graph(super::DEFAULT_TONE);
        let s = crate::material::Samples::tile(256, 1.0);
        let c =
            crate::material::bake(&g.nodes, &g.params, &s, false).unwrap_or_else(|e| panic!("{e}"));
        let n = c.base_color.len() as f32;
        let m = c.base_color.iter().fold([0.0f32; 3], |a, b| {
            [a[0] + b[0] / n, a[1] + b[1] / n, a[2] + b[2] / n]
        });
        println!("mean base {m:?}");
        assert!(m[0] < 0.9 && m[0] > m[2], "{m:?}");
    }
}
