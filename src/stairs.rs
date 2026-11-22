//! `stairs` and `railing`: the two things every multi-storey level is made
//! of, as ops, with their `Block` colliders, instead of hundreds of
//! hand-placed boxes.
//!
//! A flight is one closed solid (a staircase profile extruded across its
//! width), so it takes a tile material and passes `validate`. A railing is a
//! top rail, posts and an optional mid rail along a path, merged into one
//! part. Both can make `block` markers (`<name>_blk_<i>`, the game's `Block`
//! component): one box per step, one per railing run.

use bevy::math::{Quat, Vec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{ModelerError, Part, PrimitiveKind};

/// A flight of stairs (a closed solid; the underside is filled down to its
/// foot, so it can stand on a floor or against a wall).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StairsDef {
    /// The foot: the middle of the lowest edge, on the floor the flight
    /// climbs from.
    pub from: [f32; 3],
    /// Which way it climbs: "+x", "-x", "+z", "-z", or degrees about +y
    /// (0 = toward +x, 90 = toward -z, 180 = toward -x, 270 = toward +z).
    pub toward: serde_json::Value,
    /// Total height gained, meters.
    pub rise: f32,
    /// Width across the flight, meters (default 1.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f32>,
    /// Number of steps (default: enough for a riser of at most 0.2 m).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<u32>,
    /// Depth of each tread, meters (default 0.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<f32>,
    /// "none" (default) or "blocks": one `block` marker per step
    /// (`<name>_blk_<i>`, with the game's `Block` component).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collider: Option<String>,
}

pub const STAIRS_FIELDS: &[&str] = &[
    "from", "toward", "rise", "width", "steps", "run", "collider",
];

/// A railing along a path.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RailingDef {
    /// The base line [[x, y, z], ...] (y = the floor it stands on): 2+ points.
    pub path: Vec<[f32; 3]>,
    /// Height of the top rail, meters (default 1.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<f32>,
    /// Distance between posts, meters (default 1.5; posts also stand at
    /// every corner and both ends).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_spacing: Option<f32>,
    /// Section of the rails and posts, meters (default 0.06).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thickness: Option<f32>,
    /// A second rail halfway up (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mid_rail: Option<bool>,
    /// "none" (default) or "blocks": one `block` marker per run, full height.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collider: Option<String>,
}

pub const RAILING_FIELDS: &[&str] = &[
    "path",
    "height",
    "post_spacing",
    "thickness",
    "mid_rail",
    "collider",
];

/// A derived `block` box in world space: centre, local size, yaw (degrees
/// about +y; the size is in the box's own frame).
pub type BlockBox = (Vec3, Vec3, f32);

fn direction(toward: &serde_json::Value) -> Result<f32, String> {
    match toward {
        serde_json::Value::String(s) => match s.as_str() {
            "+x" | "x" | "right" => Ok(0.0),
            "-z" | "back" => Ok(90.0),
            "-x" | "left" => Ok(180.0),
            "+z" | "z" | "front" => Ok(270.0),
            o => Err(format!(
                "toward: \"{o}\": use \"+x\", \"-x\", \"+z\", \"-z\" or degrees"
            )),
        },
        serde_json::Value::Number(n) => n
            .as_f64()
            .map(|d| d as f32)
            .ok_or_else(|| "toward: not a number".to_string()),
        _ => Err("toward: \"+x\", \"-x\", \"+z\", \"-z\" or degrees".into()),
    }
}

pub fn validate_stairs(d: &StairsDef) -> Result<(), String> {
    direction(&d.toward)?;
    if !d.rise.is_finite() || d.rise <= 0.0 {
        return Err("rise: must be > 0 meters".into());
    }
    if d.from.iter().any(|c| !c.is_finite()) {
        return Err("from: must be finite".into());
    }
    if d.width.is_some_and(|w| w.is_nan() || w <= 0.0)
        || d.run.is_some_and(|r| r.is_nan() || r <= 0.0)
        || d.steps == Some(0)
    {
        return Err("width and run must be > 0, steps >= 1".into());
    }
    if let Some(c) = &d.collider
        && !["none", "blocks"].contains(&c.as_str())
    {
        return Err(format!("collider: must be none or blocks, got `{c}`"));
    }
    Ok(())
}

pub fn validate_railing(d: &RailingDef) -> Result<(), String> {
    if d.path.len() < 2 || d.path.iter().flatten().any(|c| !c.is_finite()) {
        return Err("path: needs 2+ finite [x, y, z] points".into());
    }
    if d.height.is_some_and(|h| h.is_nan() || h <= 0.0)
        || d.post_spacing.is_some_and(|h| h.is_nan() || h <= 0.0)
        || d.thickness.is_some_and(|h| h.is_nan() || h <= 0.0)
    {
        return Err("height, post_spacing and thickness must be > 0".into());
    }
    if let Some(c) = &d.collider
        && !["none", "blocks"].contains(&c.as_str())
    {
        return Err(format!("collider: must be none or blocks, got `{c}`"));
    }
    Ok(())
}

fn flight(d: &StairsDef) -> (u32, f32, f32, f32) {
    let steps = d
        .steps
        .unwrap_or_else(|| (d.rise / 0.2).ceil().max(1.0) as u32);
    (
        steps,
        d.rise / steps as f32,
        d.run.unwrap_or(0.3),
        d.width.unwrap_or(1.2),
    )
}

/// The flight as one closed solid.
pub fn build_stairs(name: &str, d: &StairsDef) -> Result<Part, ModelerError> {
    validate_stairs(d).map_err(|e| ModelerError::Invalid(format!("stairs `{name}`: {e}")))?;
    let yaw = direction(&d.toward).map_err(ModelerError::Invalid)?;
    let (n, riser, run, width) = flight(d);
    // Side profile [along, height]: up each riser, along each tread, then
    // back down the far end.
    let mut ring: Vec<[f32; 2]> = vec![[0.0, 0.0]];
    for i in 0..n {
        ring.push([i as f32 * run, riser * (i + 1) as f32]);
        ring.push([(i + 1) as f32 * run, riser * (i + 1) as f32]);
    }
    ring.push([n as f32 * run, 0.0]);
    let mut p = crate::csg::extrude(name, &[ring], width, 0.0, [1.0, 1.0], 0, 0.0)?;
    // Stand the profile up (footprint z -> height, thickness -> -z) and turn it.
    let r = Quat::from_rotation_y(yaw.to_radians())
        * Quat::from_axis_angle(Vec3::X, -std::f32::consts::FRAC_PI_2);
    let ryaw = Quat::from_rotation_y(yaw.to_radians());
    p.rotation = r.to_array();
    p.translation = (Vec3::from(d.from) + ryaw * Vec3::new(0.0, 0.0, width * 0.5)).to_array();
    Ok(p)
}

/// One box per step, for `collider: "blocks"`.
pub fn stairs_blocks(d: &StairsDef) -> Vec<BlockBox> {
    let Ok(yaw) = direction(&d.toward) else {
        return Vec::new();
    };
    let (n, riser, run, width) = flight(d);
    let ryaw = Quat::from_rotation_y(yaw.to_radians());
    (0..n)
        .map(|i| {
            let h = riser * (i + 1) as f32;
            let local = Vec3::new((i as f32 + 0.5) * run, h * 0.5, 0.0);
            (
                Vec3::from(d.from) + ryaw * local,
                Vec3::new(run, h, width),
                yaw,
            )
        })
        .collect()
}

fn bake_box(out: &mut Part, size: Vec3, centre: Vec3, yaw: f32, pitch: f32) {
    let Ok(mut b) = crate::primitives::primitive(PrimitiveKind::Box, size.to_array(), "b") else {
        return;
    };
    let q = Quat::from_rotation_y(yaw) * Quat::from_rotation_z(pitch);
    let base = out.verts.len() as u32;
    for v in &b.verts {
        out.verts.push((q * Vec3::from(*v) + centre).to_array());
    }
    for n in &b.normals {
        out.normals.push((q * Vec3::from(*n)).to_array());
    }
    out.uvs.append(&mut b.uvs);
    out.indices.extend(b.indices.iter().map(|i| i + base));
}

/// The segments of a railing: (start, end) on the base line, with posts at
/// every corner and end and along long runs.
fn posts(d: &RailingDef) -> Vec<Vec3> {
    let sp = d.post_spacing.unwrap_or(1.5);
    let pts: Vec<Vec3> = d.path.iter().map(|p| Vec3::from(*p)).collect();
    let mut out = vec![pts[0]];
    for w in pts.windows(2) {
        let len = w[0].distance(w[1]);
        let n = (len / sp).ceil().max(1.0) as usize;
        for k in 1..=n {
            out.push(w[0].lerp(w[1], k as f32 / n as f32));
        }
    }
    out
}

/// The railing as one merged part.
pub fn build_railing(name: &str, d: &RailingDef) -> Result<Part, ModelerError> {
    validate_railing(d).map_err(|e| ModelerError::Invalid(format!("railing `{name}`: {e}")))?;
    let (h, t) = (d.height.unwrap_or(1.0), d.thickness.unwrap_or(0.06));
    let mut out = Part::new(name);
    for p in posts(d) {
        bake_box(
            &mut out,
            Vec3::new(t, h, t),
            p + Vec3::new(0.0, h * 0.5, 0.0),
            0.0,
            0.0,
        );
    }
    let mid = d.mid_rail.unwrap_or(true);
    for w in d.path.windows(2) {
        let (a, b) = (Vec3::from(w[0]), Vec3::from(w[1]));
        let dv = b - a;
        let yaw = (-dv.z).atan2(dv.x);
        let flat = (dv.x * dv.x + dv.z * dv.z).sqrt();
        let pitch = dv.y.atan2(flat);
        let len = dv.length();
        let mut heights = vec![h - t * 0.5];
        if mid {
            heights.push(h * 0.5);
        }
        for hh in heights {
            bake_box(
                &mut out,
                Vec3::new(len, t, t),
                (a + b) * 0.5 + Vec3::new(0.0, hh, 0.0),
                yaw,
                pitch,
            );
        }
    }
    Ok(out)
}

/// One box per run, for `collider: "blocks"`.
pub fn railing_blocks(d: &RailingDef) -> Vec<BlockBox> {
    let (h, t) = (
        d.height.unwrap_or(1.0),
        d.thickness.unwrap_or(0.06).max(0.1),
    );
    d.path
        .windows(2)
        .map(|w| {
            let (a, b) = (Vec3::from(w[0]), Vec3::from(w[1]));
            let dv = b - a;
            let yaw = (-dv.z).atan2(dv.x).to_degrees();
            let flat = (dv.x * dv.x + dv.z * dv.z).sqrt();
            (
                (a + b) * 0.5 + Vec3::new(0.0, h * 0.5, 0.0),
                Vec3::new(flat.max(0.01) + t, h, t),
                yaw,
            )
        })
        .collect()
}

/// `block` markers for a set of boxes.
pub fn block_names(name: &str, n: usize) -> Vec<String> {
    (0..n).map(|i| format!("{name}_blk_{i}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(toward: &str) -> StairsDef {
        StairsDef {
            from: [2.0, 0.5, 3.0],
            toward: serde_json::Value::String(toward.into()),
            rise: 2.0,
            width: Some(1.0),
            steps: Some(10),
            run: Some(0.3),
            collider: None,
        }
    }

    #[test]
    fn a_flight_climbs_the_rise_in_the_direction_asked() {
        for (t, dx, dz) in [
            ("+x", 3.0, 0.0),
            ("-x", -3.0, 0.0),
            ("+z", 0.0, 3.0),
            ("-z", 0.0, -3.0),
        ] {
            let p = build_stairs("s", &st(t)).unwrap_or_else(|e| panic!("{e}"));
            let (lo, hi) = p.bounds();
            assert!(
                (hi.y - 2.5).abs() < 1e-3 && (lo.y - 0.5).abs() < 1e-3,
                "{t}: y {lo} {hi}"
            );
            // The far end of the flight is 10 treads away along the direction.
            let far_x = if dx != 0.0 {
                if dx > 0.0 { hi.x } else { lo.x }
            } else {
                (lo.x + hi.x) * 0.5
            };
            let far_z = if dz != 0.0 {
                if dz > 0.0 { hi.z } else { lo.z }
            } else {
                (lo.z + hi.z) * 0.5
            };
            assert!(
                (far_x - (2.0 + dx)).abs() < 1e-3,
                "{t}: x {far_x} {lo} {hi}"
            );
            assert!(
                (far_z - (3.0 + dz)).abs() < 1e-3,
                "{t}: z {far_z} {lo} {hi}"
            );
            // Centred across its width.
            let across = if dx != 0.0 {
                (lo.z + hi.z) * 0.5 - 3.0
            } else {
                (lo.x + hi.x) * 0.5 - 2.0
            };
            assert!(across.abs() < 1e-3, "{t}: across {across}");
        }
    }

    #[test]
    fn step_blocks_match_the_solid() {
        let d = st("+x");
        let b = stairs_blocks(&d);
        assert_eq!(b.len(), 10);
        let top = b.last().map(|(c, s, _)| c.y + s.y * 0.5).unwrap_or(0.0);
        assert!((top - 2.5).abs() < 1e-4, "{top}");
    }

    #[test]
    fn a_railing_has_rails_posts_and_one_block_per_run() {
        let d = RailingDef {
            path: vec![[0.0, 0.0, 0.0], [4.0, 0.0, 0.0], [4.0, 0.0, 3.0]],
            ..Default::default()
        };
        let p = build_railing("r", &d).unwrap_or_else(|e| panic!("{e}"));
        let (lo, hi) = p.bounds();
        assert!((hi.y - 1.0).abs() < 1e-3 && lo.y > -1e-3, "{lo} {hi}");
        assert_eq!(railing_blocks(&d).len(), 2);
        assert!(validate_railing(&RailingDef::default()).is_err());
    }
}
