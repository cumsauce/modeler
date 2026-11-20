//! Clothing generated from a body's own surface (`garment` items).
//!
//! A garment is cut out of the body it is `on`: a scalar field over the
//! body's (welded) vertices says how far inside the region each one is
//! (body zones from the skin weights, arm / leg extents along the limb
//! joints, heights, planes, boxes), the triangles are clipped along the
//! field's zero line (an even hem, not a zig-zag of whole faces), and the
//! piece is offset along the smooth normals. `ease` relaxes it so it
//! bridges concavities instead of hugging them (pushed back out to at
//! least `offset` over its own source point), a hem can flare, and a
//! `thickness` makes it a closed shell (inner layer at `offset`, outer at
//! `offset + thickness`, stitched along every border). Skirts and dresses
//! bridge the legs with a tube built from the convex hull of the body's
//! horizontal slices; shoes add a flat sole on y = 0.
//!
//! Every body face the garment fully covers (away from its borders) is
//! remembered ([`remember`]); [`mask_covered`] drops those faces from
//! the body's index list (vertices kept, so skin weights, morphs and
//! correctives keep their counts) while the garment is visible, the
//! standard game trick against poke-through. The body op is unchanged:
//! hide or delete the garment and the skin comes back.

use crate::{Part, name_matches};
use bevy::math::{Vec2, Vec3};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// A garment item's fields (besides `name`).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, Default, PartialEq)]
pub struct GarmentDef {
    /// The closed body part (or another garment, for layers) it is built on.
    pub on: String,
    /// A ready recipe on a `human` body: tshirt, longsleeve, tank, trousers,
    /// shorts, skirt, dress, socks, shoes, gloves, hood, briefs, bra. Every
    /// other field overrides it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    /// Meters above the surface it is on (default 0.006).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<f32>,
    /// Cloth thickness in meters: 0 = one open sheet, > 0 a closed shell
    /// (default 0.003).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thickness: Option<f32>,
    /// Where it covers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<Region>,
    /// 0 skin-tight .. 1 bridges hollows like loose cloth (up to 3: merges
    /// toes or fingers, shoes and mittens).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ease: Option<f32>,
    /// Hem shape at every border.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hem: Option<Hem>,
    /// Hide the body faces it covers (default true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hide_skin: Option<bool>,
    /// Bind it to the body's rig with the body's weights (default true when
    /// the body has a rig).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bind: Option<bool>,
    /// Skirt / dress hem height (meters, or a joint "knee_l-0.05") for the
    /// leg-bridging tube.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skirt: Option<Skirt>,
}

/// Where a garment covers: everything given must hold (zones are a union).
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Region {
    /// Human body zones: head, neck, torso, hips, arms, hands, legs, feet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub zones: Vec<String>,
    /// Sleeves: none, cap, short, elbow, forearm, wrist, or [from, to]
    /// along the arm (0 shoulder, 1 elbow, 2 wrist).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arms: Option<Extent>,
    /// Legs: none, short, knee, calf, ankle, or [from, to] (0 hip joint,
    /// 1 knee, 2 ankle).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legs: Option<Extent>,
    /// Neckline: crew (default), high (includes the neck), or v (or the v's
    /// depth in meters).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub neck: Option<Extent>,
    /// Keep world y above this: meters, or a joint name with an offset
    /// ("hip-0.05").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub above: Option<Level>,
    /// Keep world y below this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub below: Option<Level>,
    /// Keep the side each normal points to.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub planes: Vec<Plane>,
    /// Keep inside these boxes (a union).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub boxes: Vec<Aabb>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(untagged)]
pub enum Extent {
    Word(String),
    Range([f32; 2]),
    Num(f32),
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(untagged)]
pub enum Level {
    Num(f32),
    Joint(String),
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Plane {
    pub normal: [f32; 3],
    pub point: [f32; 3],
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Aabb {
    pub min: [f32; 3],
    pub max: [f32; 3],
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Hem {
    /// Meters the border flares out.
    #[serde(default)]
    pub flare: f32,
    /// Meters from the border the flare fades over (default 0.06).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub length: Option<f32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Skirt {
    /// The waistband height (meters or "joint+m").
    pub top: Level,
    /// The hem height.
    pub hem: Level,
    /// How far the hem flares out beyond the legs' hull (default 0.08).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flare: Option<f32>,
}

/// The fields of a garment item besides `name` and `type`.
pub const FIELDS: &[&str] = &[
    "on",
    "preset",
    "offset",
    "thickness",
    "region",
    "ease",
    "hem",
    "hide_skin",
    "bind",
    "skirt",
];

pub const PRESETS: &[&str] = &[
    "tshirt",
    "longsleeve",
    "tank",
    "trousers",
    "shorts",
    "skirt",
    "dress",
    "socks",
    "shoes",
    "gloves",
    "hood",
    "briefs",
    "bra",
    "hair_buzz",
    "hair_short",
    "hair_bob",
];

fn word(s: &str) -> Option<Extent> {
    Some(Extent::Word(s.into()))
}
fn lv(s: &str) -> Option<Level> {
    Some(Level::Joint(s.into()))
}

/// The preset's recipe (region, offset, ease, hem, colour).
fn preset(name: &str) -> Result<(GarmentDef, [&'static str; 2], f32), String> {
    let z = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let mut d = GarmentDef::default();
    let reg = |zones: &[&str]| Region {
        zones: z(zones),
        ..Region::default()
    };
    let (cols, rough) = match name {
        "tshirt" | "longsleeve" | "tank" => {
            let mut r = reg(&["torso", "hips"]);
            r.arms = match name {
                "tshirt" => word("short"),
                "longsleeve" => word("wrist"),
                _ => word("none"),
            };
            r.above = lv("hip-0.02");
            d.region = Some(r);
            // Over the waistband of trousers (0.005 + 0.003).
            d.offset = Some(0.010);
            d.ease = Some(0.25);
            d.hem = Some(Hem {
                flare: 0.004,
                length: None,
            });
            (["#d9d4c8", "#ebe7dc"], 0.9)
        }
        "trousers" | "shorts" => {
            let mut r = reg(&["hips"]);
            r.legs = if name == "trousers" {
                word("ankle")
            } else {
                word("short")
            };
            r.below = lv("hip+0.07");
            d.region = Some(r);
            d.offset = Some(0.005);
            d.ease = Some(0.6);
            (["#27354f", "#3d5276"], 0.85)
        }
        "briefs" => {
            let mut r = reg(&["hips"]);
            r.legs = Some(Extent::Range([0.0, 0.12]));
            r.below = lv("hip+0.04");
            d.region = Some(r);
            d.offset = Some(0.003);
            d.ease = Some(0.3);
            (["#2c2c30", "#3a3a40"], 0.8)
        }
        "bra" => {
            let mut r = reg(&["torso"]);
            r.arms = word("none");
            r.above = lv("chest+0.03");
            r.below = lv("chest+0.17");
            d.region = Some(r);
            d.offset = Some(0.003);
            d.ease = Some(0.8);
            (["#2c2c30", "#3a3a40"], 0.7)
        }
        "socks" => {
            let mut r = reg(&["feet"]);
            r.legs = Some(Extent::Range([1.55, 2.0]));
            d.region = Some(r);
            d.offset = Some(0.002);
            d.ease = Some(0.2);
            (["#e8e6e0", "#f4f2ec"], 0.95)
        }
        "shoes" => {
            let mut r = reg(&["feet"]);
            r.legs = Some(Extent::Range([1.88, 2.0]));
            r.above = Some(Level::Num(0.012));
            d.region = Some(r);
            // Outside a trouser hem; eased hard so the toes merge.
            d.offset = Some(0.011);
            d.ease = Some(3.0);
            d.thickness = Some(0.004);
            (["#2a211c", "#3b2e26"], 0.55)
        }
        "gloves" => {
            let mut r = reg(&["hands"]);
            r.arms = Some(Extent::Range([1.8, 2.0]));
            d.region = Some(r);
            d.offset = Some(0.002);
            d.ease = Some(0.2);
            (["#3a2a20", "#4a362a"], 0.6)
        }
        "hood" => {
            let mut r = reg(&["head", "neck"]);
            r.planes = vec![Plane {
                normal: [0.0, 0.0, -1.0],
                point: [0.0, 0.0, f32::NAN],
            }];
            d.region = Some(r);
            d.offset = Some(0.012);
            d.ease = Some(1.0);
            (["#5a6a4a", "#6a7a58"], 0.9)
        }
        "skirt" => {
            d.skirt = Some(Skirt {
                top: Level::Joint("hip+0.07".into()),
                hem: Level::Joint("knee_l+0.06".into()),
                flare: None,
            });
            d.offset = Some(0.008);
            (["#6b2e3a", "#84404d"], 0.85)
        }
        "dress" => {
            let mut r = reg(&["torso"]);
            r.arms = word("none");
            r.above = lv("hip+0.03");
            d.region = Some(r);
            d.skirt = Some(Skirt {
                top: Level::Joint("hip+0.09".into()),
                hem: Level::Joint("knee_l-0.04".into()),
                flare: None,
            });
            d.offset = Some(0.006);
            d.ease = Some(0.7);
            (["#2f4b6b", "#3e6088"], 0.85)
        }
        "hair_buzz" | "hair_short" | "hair_bob" => {
            // The hairline planes are placed from the head joint in build.
            d.region = Some(reg(&["head"]));
            let (off, ease) = match name {
                "hair_buzz" => (0.0015, 0.0),
                "hair_short" => (0.010, 0.8),
                _ => (0.016, 1.0),
            };
            d.offset = Some(off);
            d.ease = Some(ease);
            d.thickness = Some(0.004);
            (["#2b1d14", "#4a3322"], 0.6)
        }
        other => {
            return Err(format!(
                "preset: unknown `{other}` (one of: {})",
                PRESETS.join(", ")
            ));
        }
    };
    Ok((d, cols, rough))
}

/// The def with its preset filled in under the given fields, plus the
/// default fabric look.
fn resolved(def: &GarmentDef) -> Result<(GarmentDef, [&'static str; 2], f32), String> {
    let (mut base, cols, rough) = match def.preset.as_deref() {
        Some(p) => preset(p)?,
        None => (GarmentDef::default(), ["#7a7a80", "#8a8a90"], 0.85),
    };
    base.on = def.on.clone();
    macro_rules! over {
        ($($f:ident),*) => { $( if def.$f.is_some() { base.$f = def.$f.clone(); } )* };
    }
    over!(offset, thickness, ease, hem, hide_skin, bind, skirt);
    if let Some(r) = &def.region {
        // Region fields override the preset's one by one.
        let mut b = base.region.take().unwrap_or_default();
        if !r.zones.is_empty() {
            b.zones = r.zones.clone();
        }
        macro_rules! ro {
            ($($f:ident),*) => { $( if r.$f.is_some() { b.$f = r.$f.clone(); } )* };
        }
        ro!(arms, legs, neck, above, below);
        if !r.planes.is_empty() {
            b.planes = r.planes.clone();
        }
        if !r.boxes.is_empty() {
            b.boxes = r.boxes.clone();
        }
        base.region = Some(b);
    }
    Ok((base, cols, rough))
}

/// Checks the fields that need no body (parse time).
pub fn validate(def: &GarmentDef) -> Result<(), String> {
    let (d, _, _) = resolved(def)?;
    if d.region.is_none() && d.skirt.is_none() {
        return Err("needs a `region` (zones, arms, legs, above, below, planes, boxes), a `skirt` or a `preset`".into());
    }
    let off = d.offset.unwrap_or(0.006);
    if !(0.0..=0.2).contains(&off) {
        return Err(format!("offset: {off} m must be 0..0.2"));
    }
    let th = d.thickness.unwrap_or(0.003);
    if !(0.0..=0.05).contains(&th) {
        return Err(format!("thickness: {th} m must be 0..0.05"));
    }
    if let Some(r) = &d.region {
        for z in &r.zones {
            if zone_of_word(z).is_none() {
                return Err(format!(
                    "region.zones: unknown `{z}` (head, neck, torso, hips, arms, hands, legs, feet)"
                ));
            }
        }
        if let Some(e) = &r.arms {
            extent(e, true)?;
        }
        if let Some(e) = &r.legs {
            extent(e, false)?;
        }
    }
    Ok(())
}

/// The name of the body at the bottom of a garment stack, for the bind.
pub fn root_body<'a>(ops: &'a [crate::Op], on: &'a str) -> Option<&'a str> {
    let mut cur = on;
    for _ in 0..16 {
        match ops.iter().find_map(|o| match o {
            crate::Op::Garment { name, def } if name == cur => Some(def.on.as_str()),
            _ => None,
        }) {
            Some(next) => cur = next,
            None => return Some(cur),
        }
    }
    None
}

/// The implicit binds garments bring (`<name>_bind`: smooth, weights from
/// the part it is on), for the rig's bind list. Garments whose root body
/// has no rig (not a `body` op, or `rig: false`) and `bind: false` ones
/// get none. Layers come after what they are on, so sources bind first.
pub fn implicit_binds(ops: &[crate::Op]) -> Vec<(String, crate::anim::BindDef)> {
    let mut out = Vec::new();
    for o in ops {
        let crate::Op::Garment { name, def } = o else {
            continue;
        };
        if def.bind == Some(false) {
            continue;
        }
        let Some(root) = root_body(ops, &def.on) else {
            continue;
        };
        let rigged = ops.iter().any(|o| {
            matches!(o, crate::Op::Body { name: n, def: b } if n == root && b.rig != Some(false))
        });
        if !rigged {
            continue;
        }
        let bind = format!("{name}_bind");
        if ops
            .iter()
            .any(|o| matches!(o, crate::Op::Bind { name: n, .. } if *n == bind))
        {
            continue;
        }
        out.push((
            bind,
            crate::anim::BindDef {
                skeleton: format!("{root}_rig"),
                parts: vec![name.clone()],
                bone: None,
                mode: "smooth".into(),
                blend: def.skirt.as_ref().map(|_| 0.12),
                paint: Vec::new(),
                weights_from: Some(def.on.clone()),
                correctives: None,
            },
        ));
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Zone {
    Head,
    Neck,
    Torso,
    Hips,
    Arm,
    Hand,
    Leg,
    Foot,
}

fn zone_of_word(w: &str) -> Option<Zone> {
    Some(match w {
        "head" => Zone::Head,
        "neck" => Zone::Neck,
        "torso" | "chest" => Zone::Torso,
        "hips" | "pelvis" => Zone::Hips,
        "arms" => Zone::Arm,
        "hands" => Zone::Hand,
        "legs" => Zone::Leg,
        "feet" => Zone::Foot,
        _ => return None,
    })
}

/// The zone a rig bone's weights belong to.
fn zone_of_bone(b: &str) -> Zone {
    let s = b
        .strip_suffix("_l")
        .or_else(|| b.strip_suffix("_r"))
        .unwrap_or(b);
    let s = s.trim_end_matches(|c: char| c.is_ascii_digit() || c == '_');
    match s {
        "head" | "jaw" | "eye" | "tongue" => Zone::Head,
        "neck" => Zone::Neck,
        "hip" | "root" | "pelvis" => Zone::Hips,
        "shoulder" | "elbow" => Zone::Arm,
        "wrist" | "hand" | "thumb" | "index" | "middle" | "ring" | "pinky" => Zone::Hand,
        "thigh" | "knee" => Zone::Leg,
        "ankle" | "ball" | "toe" => Zone::Foot,
        _ => Zone::Torso,
    }
}

/// An arm / leg extent as [from, to] along the limb (0 root joint, 1 the
/// middle joint, 2 the end joint); `None` = the limb is left out.
fn extent(e: &Extent, arm: bool) -> Result<Option<[f32; 2]>, String> {
    Ok(match e {
        Extent::Range(r) => Some(*r),
        Extent::Num(n) => Some([0.0, *n]),
        Extent::Word(w) => {
            let to = match (arm, w.as_str()) {
                (_, "none") => return Ok(None),
                (true, "cap") => 0.22,
                (true, "short") => 0.5,
                (true, "elbow") => 1.0,
                (true, "forearm") => 1.5,
                (true, "wrist") | (true, "full") => 2.0,
                (false, "short") => 0.45,
                (false, "knee") => 1.0,
                (false, "calf") => 1.5,
                (false, "ankle") | (false, "full") => 2.0,
                _ => {
                    return Err(format!(
                        "{}: unknown `{w}` ({})",
                        if arm { "arms" } else { "legs" },
                        if arm {
                            "none, cap, short, elbow, forearm, wrist or [from, to]"
                        } else {
                            "none, short, knee, calf, ankle or [from, to]"
                        }
                    ));
                }
            };
            Some([-1.0, to])
        }
    })
}

/// A height: meters, or a joint of `body` with an optional +/- offset.
fn level(l: &Level, body: &Part) -> Result<f32, String> {
    match l {
        Level::Num(v) => Ok(*v),
        Level::Joint(s) => {
            let cut = s[1..].find(['+', '-']).map(|i| i + 1);
            let (j, off) = match cut {
                Some(i) => (
                    &s[..i],
                    s[i..].parse::<f32>().map_err(|_| {
                        format!("`{s}`: the offset after the joint is not a number")
                    })?,
                ),
                None => (s.as_str(), 0.0),
            };
            let p = body.joint(j.trim()).ok_or_else(|| {
                let known: Vec<String> = body
                    .joints
                    .as_ref()
                    .map(|v| v.iter().map(|x| x.0.clone()).collect())
                    .unwrap_or_default();
                format!(
                    "`{s}`: `{}` has no joint `{j}`{}",
                    body.name,
                    if known.is_empty() {
                        " (it has no joints: give meters)".to_string()
                    } else {
                        format!(" (joints: {})", known.join(", "))
                    }
                )
            })?;
            Ok(p.y + off)
        }
    }
}

/// The body welded by position: groups, positions, smooth normals.
struct Welded {
    first: Vec<u32>,
    pos: Vec<Vec3>,
    nrm: Vec<Vec3>,
    tris: Vec<[u32; 3]>,
    /// Raw triangle index of each welded triangle.
    raw_tri: Vec<u32>,
}

fn weld(body: &Part) -> Welded {
    let n = body.verts.len();
    let mut map: HashMap<[i64; 3], u32> = HashMap::new();
    let mut group = Vec::with_capacity(n);
    let mut first = Vec::new();
    let mut pos = Vec::new();
    for i in 0..n {
        let p = body.world_vert(i);
        let k = (p * 2e5).round();
        let key = [k.x as i64, k.y as i64, k.z as i64];
        let g = *map.entry(key).or_insert_with(|| {
            first.push(i as u32);
            pos.push(p);
            (pos.len() - 1) as u32
        });
        group.push(g);
    }
    let group = group;
    let mut tris = Vec::new();
    let mut raw_tri = Vec::new();
    let mut nrm = vec![Vec3::ZERO; pos.len()];
    for (t, tri) in body.indices.as_chunks::<3>().0.iter().enumerate() {
        let g = tri.map(|i| group[i as usize]);
        if g[0] == g[1] || g[1] == g[2] || g[0] == g[2] {
            continue;
        }
        let [a, b, c] = g.map(|i| pos[i as usize]);
        let fnrm = (b - a).cross(c - a);
        for &i in &g {
            nrm[i as usize] += fnrm;
        }
        tris.push(g);
        raw_tri.push(t as u32);
    }
    for v in &mut nrm {
        *v = v.normalize_or(Vec3::Y);
    }
    Welded {
        first,
        pos,
        nrm,
        tris,
        raw_tri,
    }
}

/// Distance-along-limb parameter of `p` over joints a -> b -> c (0..2)
/// and the distance to that chain.
fn limb_param(p: Vec3, a: Vec3, b: Vec3, c: Vec3) -> (f32, f32) {
    let seg = |p: Vec3, a: Vec3, b: Vec3| {
        let ab = b - a;
        let t = ((p - a).dot(ab) / ab.length_squared().max(1e-9)).clamp(0.0, 1.0);
        (t, (a + ab * t - p).length())
    };
    let (t1, d1) = seg(p, a, b);
    let (t2, d2) = seg(p, b, c);
    if d1 <= d2 { (t1, d1) } else { (1.0 + t2, d2) }
}

/// The region field over the welded vertices (>= 0 inside).
fn field(body: &Part, w: &Welded, r: &Region) -> Result<Vec<f32>, String> {
    let n = w.pos.len();
    let zones: Vec<Zone> = r.zones.iter().filter_map(|z| zone_of_word(z)).collect();
    let arms = match &r.arms {
        Some(e) => extent(e, true)?,
        None => None,
    };
    let legs = match &r.legs {
        Some(e) => extent(e, false)?,
        None => None,
    };
    let high_neck = matches!(&r.neck, Some(Extent::Word(s)) if s == "high" || s == "turtle");
    let v_depth = match &r.neck {
        Some(Extent::Word(s)) if s == "v" => Some(0.12),
        Some(Extent::Num(d)) => Some(*d),
        _ => None,
    };
    let use_zones = !zones.is_empty() || arms.is_some() || legs.is_some();
    let skin = if use_zones {
        Some(body.skin.as_ref().ok_or_else(|| {
            format!(
                "region zones / arms / legs need a body with skin weights (a `human` body); `{}` has none: use above, below, planes or boxes",
                body.name
            )
        })?)
    } else {
        None
    };
    let bone_zone: Vec<Zone> = skin
        .map(|s| s.bones.iter().map(|b| zone_of_bone(b)).collect())
        .unwrap_or_default();
    let joint = |n: &str| -> Result<Vec3, String> {
        body.joint(n).ok_or_else(|| {
            format!(
                "`{}` has no joint `{n}` (needed for arms / legs)",
                body.name
            )
        })
    };
    let limb = |names: [&str; 3]| -> Result<[[Vec3; 3]; 2], String> {
        let l = [
            joint(&format!("{}_l", names[0]))?,
            joint(&format!("{}_l", names[1]))?,
            joint(&format!("{}_l", names[2]))?,
        ];
        let rr = [
            joint(&format!("{}_r", names[0]))?,
            joint(&format!("{}_r", names[1]))?,
            joint(&format!("{}_r", names[2]))?,
        ];
        Ok([l, rr])
    };
    let arm_j = if arms.is_some() {
        Some(limb(["shoulder", "elbow", "wrist"])?)
    } else {
        None
    };
    let leg_j = if legs.is_some() {
        Some(limb(["thigh", "knee", "ankle"])?)
    } else {
        None
    };
    let above = r.above.as_ref().map(|l| level(l, body)).transpose()?;
    let below = r.below.as_ref().map(|l| level(l, body)).transpose()?;
    let neck_y = if v_depth.is_some() {
        Some(joint("neck")?)
    } else {
        None
    };
    // A plane point with NaN z: the head joint's (hood preset).
    let head = body.joint("head");
    let mut out = Vec::with_capacity(n);
    for g in 0..n {
        let p = w.pos[g];
        let mut s = f32::INFINITY;
        if let Some(sk) = skin {
            let raw = w.first[g] as usize;
            let (j, wt) = (
                sk.joints.get(raw).copied().unwrap_or_default(),
                sk.weights.get(raw).copied().unwrap_or_default(),
            );
            let mut wz = [0.0f32; 8];
            for k in 0..4 {
                if let Some(z) = bone_zone.get(j[k] as usize) {
                    wz[*z as usize] += wt[k];
                }
            }
            let mut sum: f32 = zones.iter().map(|z| wz[*z as usize]).sum();
            if arms.is_some() && !zones.contains(&Zone::Arm) {
                sum += wz[Zone::Arm as usize];
            }
            if legs.is_some() && !zones.contains(&Zone::Leg) {
                sum += wz[Zone::Leg as usize];
            }
            if high_neck && !zones.contains(&Zone::Neck) {
                sum += wz[Zone::Neck as usize];
            }
            s = sum - 0.5;
            for (ext, js, z) in [(arms, arm_j, Zone::Arm), (legs, leg_j, Zone::Leg)] {
                let (Some([a0, a1]), Some(js)) = (ext, js) else {
                    continue;
                };
                let side = if p.x >= 0.0 { &js[0] } else { &js[1] };
                let (u, _) = limb_param(p, side[0], side[1], side[2]);
                let wl = wz[z as usize];
                let gate = (1.0 - wl) * 4.0;
                s = s.min((a1 - u) + gate);
                // The start of a cuff / sock is cut everywhere: whatever
                // sits before it on the limb chain (a crotch, a shoulder)
                // is out.
                if a0 > -0.5 {
                    s = s.min(u - a0);
                }
            }
        }
        if let Some(a) = above {
            s = s.min((p.y - a) * 10.0);
        }
        if let Some(b) = below {
            s = s.min((b - p.y) * 10.0);
        }
        if let (Some(d), Some(nk)) = (v_depth, neck_y)
            && p.z > nk.z - 0.02
        {
            // Points above the V (front only) are cut away.
            let half = 0.09;
            let line = nk.y - d + (p.x.abs() / half).min(1.0) * d;
            s = s.min((line - p.y) * 10.0 + if p.z < nk.z { 0.2 } else { 0.0 });
        }
        for pl in &r.planes {
            let mut pt = Vec3::from(pl.point);
            if pt.z.is_nan() {
                pt.z = head.map_or(0.0, |h| h.z + 0.07);
            }
            let nn = Vec3::from(pl.normal).normalize_or(Vec3::Y);
            s = s.min((p - pt).dot(nn) * 10.0);
        }
        if !r.boxes.is_empty() {
            let mut best = f32::NEG_INFINITY;
            for b in &r.boxes {
                let lo = Vec3::from(b.min);
                let hi = Vec3::from(b.max);
                let d = (p - lo).min(hi - p).min_element();
                best = best.max(d * 10.0);
            }
            s = s.min(best);
        }
        out.push(s);
    }
    Ok(out)
}

/// A garment mesh under construction.
#[derive(Default)]
struct Sheet {
    base: Vec<Vec3>,
    nrm: Vec<Vec3>,
    /// The welded body vertex it came from (originals only).
    orig: Vec<Option<u32>>,
    /// Welded source vertices and blend (a, b, t) for the skin weights.
    src: Vec<(u32, u32, f32)>,
    tris: Vec<[u32; 3]>,
}

fn clip(w: &Welded, s: &[f32]) -> (Sheet, Vec<u32>) {
    let mut sh = Sheet::default();
    let mut of_orig: HashMap<u32, u32> = HashMap::new();
    let mut of_edge: HashMap<(u32, u32), u32> = HashMap::new();
    let mut covered_candidates = Vec::new();
    let mut vert = |sh: &mut Sheet, g: u32| -> u32 {
        *of_orig.entry(g).or_insert_with(|| {
            sh.base.push(w.pos[g as usize]);
            sh.nrm.push(w.nrm[g as usize]);
            sh.orig.push(Some(g));
            sh.src.push((g, g, 0.0));
            (sh.base.len() - 1) as u32
        })
    };
    for (ti, t) in w.tris.iter().enumerate() {
        let sv = t.map(|g| s[g as usize]);
        let inside = sv.map(|x| x >= 0.0);
        if inside.iter().all(|b| !b) {
            continue;
        }
        if inside.iter().all(|b| *b) {
            let v = t.map(|g| vert(&mut sh, g));
            sh.tris.push(v);
            covered_candidates.push(ti as u32);
            continue;
        }
        let mut poly: Vec<u32> = Vec::with_capacity(4);
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            let (sa, sb) = (sv[k], sv[(k + 1) % 3]);
            if sa >= 0.0 {
                poly.push(vert(&mut sh, a));
            }
            if (sa >= 0.0) != (sb >= 0.0) {
                let key = (a.min(b), a.max(b));
                let id = *of_edge.entry(key).or_insert_with(|| {
                    let (s0, s1) = if a < b { (sa, sb) } else { (sb, sa) };
                    // Kept off the ends: a cut vertex on top of a kept one makes a
                    // sliver edge that any skinning stretches many times over.
                    let tt = (s0 / (s0 - s1)).clamp(0.2, 0.8);
                    let (p0, p1) = (w.pos[key.0 as usize], w.pos[key.1 as usize]);
                    let (n0, n1) = (w.nrm[key.0 as usize], w.nrm[key.1 as usize]);
                    sh.base.push(p0.lerp(p1, tt));
                    sh.nrm.push(n0.lerp(n1, tt).normalize_or(Vec3::Y));
                    sh.orig.push(None);
                    sh.src.push((key.0, key.1, tt));
                    (sh.base.len() - 1) as u32
                });
                poly.push(id);
            }
        }
        for k in 1..poly.len().saturating_sub(1) {
            let tri = [poly[0], poly[k], poly[k + 1]];
            if tri[0] != tri[1] && tri[1] != tri[2] && tri[0] != tri[2] {
                sh.tris.push(tri);
            }
        }
    }
    (sh, covered_candidates)
}

fn neighbours(n: usize, tris: &[[u32; 3]]) -> (Vec<Vec<u32>>, Vec<bool>) {
    let mut count: HashMap<(u32, u32), u32> = HashMap::new();
    let mut nb: Vec<Vec<u32>> = vec![Vec::new(); n];
    for t in tris {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            *count.entry((a.min(b), a.max(b))).or_default() += 1;
            if !nb[a as usize].contains(&b) {
                nb[a as usize].push(b);
            }
            if !nb[b as usize].contains(&a) {
                nb[b as usize].push(a);
            }
        }
    }
    let mut border = vec![false; n];
    for ((a, b), c) in &count {
        if *c == 1 {
            border[*a as usize] = true;
            border[*b as usize] = true;
        }
    }
    (nb, border)
}

/// Border distance (meters along edges) by Dijkstra.
fn border_distance(pos: &[Vec3], nb: &[Vec<u32>], border: &[bool]) -> Vec<f32> {
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;
    let mut d = vec![f32::INFINITY; pos.len()];
    let mut heap = BinaryHeap::new();
    for (i, b) in border.iter().enumerate() {
        if *b {
            d[i] = 0.0;
            heap.push(Reverse((0u32, i as u32)));
        }
    }
    while let Some(Reverse((dk, i))) = heap.pop() {
        let di = f32::from_bits(dk);
        if di > d[i as usize] {
            continue;
        }
        for &j in &nb[i as usize] {
            let nd = di + (pos[i as usize] - pos[j as usize]).length();
            if nd < d[j as usize] {
                d[j as usize] = nd;
                heap.push(Reverse((nd.to_bits(), j)));
            }
        }
    }
    d
}

fn vertex_normals(pos: &[Vec3], tris: &[[u32; 3]]) -> Vec<Vec3> {
    let mut acc = vec![Vec3::ZERO; pos.len()];
    for t in tris {
        let [a, b, c] = t.map(|i| pos[i as usize]);
        let n = (b - a).cross(c - a);
        for &i in t {
            acc[i as usize] += n;
        }
    }
    acc.into_iter().map(|n| n.normalize_or(Vec3::Y)).collect()
}

/// Offsets, eases and hems the sheet; returns final positions.
fn shape_sheet(sh: &Sheet, offset: f32, ease: f32, hem: Option<&Hem>) -> (Vec<Vec3>, Vec<f32>) {
    let n = sh.base.len();
    let (nb, border) = neighbours(n, &sh.tris);
    let mut p: Vec<Vec3> = (0..n).map(|i| sh.base[i] + sh.nrm[i] * offset).collect();
    // Even the hem line: relax border vertices along the border.
    for _ in 0..6 {
        let prev = p.clone();
        for i in 0..n {
            if !border[i] {
                continue;
            }
            let bn: Vec<u32> = nb[i]
                .iter()
                .copied()
                .filter(|j| border[*j as usize])
                .collect();
            if bn.len() == 2 {
                let m = (prev[bn[0] as usize] + prev[bn[1] as usize]) * 0.5;
                p[i] = prev[i].lerp(m, 0.5);
            }
        }
    }
    let iters = (ease.clamp(0.0, 3.0) * 40.0).round() as usize;
    for it in 0..iters {
        let prev = p.clone();
        for i in 0..n {
            let ns: Vec<u32> = if border[i] {
                nb[i]
                    .iter()
                    .copied()
                    .filter(|j| border[*j as usize])
                    .collect()
            } else {
                nb[i].clone()
            };
            if ns.is_empty() {
                continue;
            }
            let m: Vec3 = ns.iter().map(|j| prev[*j as usize]).sum::<Vec3>() / ns.len() as f32;
            p[i] = prev[i].lerp(m, 0.5);
        }
        // Never closer than `offset` over its own source point.
        if it % 2 == 1 || it + 1 == iters {
            for i in 0..n {
                let d = (p[i] - sh.base[i]).dot(sh.nrm[i]);
                if d < offset {
                    p[i] += sh.nrm[i] * (offset - d);
                }
            }
        }
    }
    let dist = border_distance(&p, &nb, &border);
    if let Some(h) = hem.filter(|h| h.flare != 0.0) {
        let len = h.length.unwrap_or(0.06).max(1e-3);
        for i in 0..n {
            let t = (1.0 - dist[i] / len).max(0.0);
            p[i] += sh.nrm[i] * h.flare * t * t;
        }
    }
    (p, dist)
}

/// Appends a sheet as a closed shell (thickness > 0) or an open sheet.
fn emit(
    out: &mut Mesh,
    p: &[Vec3],
    tris: &[[u32; 3]],
    thickness: f32,
    src: Option<&[(u32, u32, f32)]>,
) {
    let base = out.pos.len() as u32;
    out.pos.extend_from_slice(p);
    let push_src = |out: &mut Mesh| match src {
        Some(s) => out.src.extend(s.iter().map(|x| Some(*x))),
        None => out.src.extend(std::iter::repeat_n(None, p.len())),
    };
    push_src(out);
    for t in tris {
        out.tris.push(t.map(|i| i + base));
    }
    if thickness <= 0.0 {
        return;
    }
    let n = vertex_normals(p, tris);
    let inner = out.pos.len() as u32;
    for i in 0..p.len() {
        // The outer layer moves out; the source sheet stays the inside.
        out.pos[(base as usize) + i] = p[i] + n[i] * thickness;
    }
    out.pos.extend_from_slice(p);
    push_src(out);
    for t in tris {
        out.tris.push([t[0] + inner, t[2] + inner, t[1] + inner]);
    }
    // Stitch every border edge (outer a->b, inner b'->a').
    let mut count: HashMap<(u32, u32), (u32, u32, u32)> = HashMap::new();
    for t in tris {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            let e = count.entry((a.min(b), a.max(b))).or_insert((0, a, b));
            e.0 += 1;
        }
    }
    for (_, (c, a, b)) in count {
        if c != 1 {
            continue;
        }
        let (oa, ob, ia, ib) = (a + base, b + base, a + inner, b + inner);
        out.tris.push([ob, oa, ia]);
        out.tris.push([ob, ia, ib]);
    }
}

#[derive(Default)]
struct Mesh {
    pos: Vec<Vec3>,
    tris: Vec<[u32; 3]>,
    /// Per vertex: welded body vertices and blend for the skin weights.
    src: Vec<Option<(u32, u32, f32)>>,
}

/// 2D convex hull (monotone chain), counter-clockwise.
fn hull2(mut pts: Vec<Vec2>) -> Vec<Vec2> {
    pts.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    pts.dedup();
    if pts.len() < 3 {
        return pts;
    }
    let cross = |o: Vec2, a: Vec2, b: Vec2| (a - o).perp_dot(b - o);
    let mut h: Vec<Vec2> = Vec::new();
    for &p in &pts {
        while h.len() >= 2 && cross(h[h.len() - 2], h[h.len() - 1], p) <= 0.0 {
            h.pop();
        }
        h.push(p);
    }
    let lower = h.len() + 1;
    for &p in pts.iter().rev().skip(1) {
        while h.len() >= lower && cross(h[h.len() - 2], h[h.len() - 1], p) <= 0.0 {
            h.pop();
        }
        h.push(p);
    }
    h.pop();
    h
}

/// Radius of a convex polygon around `c` toward angle `a`.
fn ray_hull(h: &[Vec2], c: Vec2, a: f32) -> f32 {
    let d = Vec2::new(a.cos(), a.sin());
    let mut best = 0.0f32;
    for k in 0..h.len() {
        let (p, q) = (h[k] - c, h[(k + 1) % h.len()] - c);
        let e = q - p;
        let den = d.perp_dot(e);
        if den.abs() < 1e-12 {
            continue;
        }
        let t = p.perp_dot(e) / den;
        let u = p.perp_dot(d) / den;
        if t > 0.0 && (-1e-4..=1.0001).contains(&u) {
            best = best.max(t);
        }
    }
    best
}

/// The skirt tube: rings from `top` to `hem` around the hull of the body's
/// slices (arms and hands left out on a human).
fn skirt_tube(
    body: &Part,
    w: &Welded,
    top: f32,
    hem: f32,
    flare: f32,
    offset: f32,
) -> Result<(Vec<Vec3>, Vec<[u32; 3]>, f32), String> {
    if top <= hem + 0.05 {
        return Err(format!(
            "skirt: top {top:.3} m must be at least 5 cm above the hem {hem:.3} m"
        ));
    }
    let arm_like: Vec<bool> = match &body.skin {
        Some(sk) => {
            let bz: Vec<Zone> = sk.bones.iter().map(|b| zone_of_bone(b)).collect();
            (0..w.pos.len())
                .map(|g| {
                    let raw = w.first[g] as usize;
                    let j = sk.joints.get(raw).copied().unwrap_or_default();
                    let wt = sk.weights.get(raw).copied().unwrap_or_default();
                    let a: f32 = (0..4)
                        .filter(|k| matches!(bz.get(j[*k] as usize), Some(Zone::Arm | Zone::Hand)))
                        .map(|k| wt[k])
                        .sum();
                    a > 0.2
                })
                .collect()
        }
        None => vec![false; w.pos.len()],
    };
    let rings = ((top - hem) / 0.02).ceil().max(3.0) as usize;
    let segs = 64usize;
    // The hull at the crotch and above: below it the legs part, the
    // skirt keeps falling from the widest hull.
    let mut prof: Vec<(f32, Vec2, Vec<f32>)> = Vec::new();
    let mut last: Option<(Vec2, Vec<f32>)> = None;
    for k in 0..=rings {
        let y = top - (top - hem) * k as f32 / rings as f32;
        let pts: Vec<Vec2> = w
            .pos
            .iter()
            .enumerate()
            .filter(|(g, p)| !arm_like[*g] && (p.y - y).abs() < 0.015)
            .map(|(_, p)| Vec2::new(p.x, p.z))
            .collect();
        let h = hull2(pts);
        let entry = if h.len() >= 3 {
            let c = h.iter().copied().sum::<Vec2>() / h.len() as f32;
            let r: Vec<f32> = (0..segs)
                .map(|j| ray_hull(&h, c, j as f32 / segs as f32 * std::f32::consts::TAU))
                .collect();
            Some((c, r))
        } else {
            None
        };
        let (c, mut r) = match (entry, &last) {
            (Some(e), _) => e,
            (None, Some(l)) => l.clone(),
            (None, None) => {
                return Err(format!(
                    "skirt: `{}` has no surface at y = {y:.3} m",
                    body.name
                ));
            }
        };
        // Cloth falls: never narrower than the ring above.
        if let Some((_, lr)) = &last {
            for j in 0..segs {
                r[j] = r[j].max(lr[j]);
            }
        }
        last = Some((c, r.clone()));
        prof.push((y, c, r));
    }
    // Smooth around each ring (the hull's corners) and down the rings.
    for (_, _, r) in &mut prof {
        for _ in 0..3 {
            let prev = r.clone();
            for j in 0..segs {
                r[j] = (prev[(j + segs - 1) % segs] + 2.0 * prev[j] + prev[(j + 1) % segs]) / 4.0;
            }
        }
    }
    // Down the rings: the hull jumps where slices catch fingers or folds.
    for _ in 0..8 {
        let prev: Vec<Vec<f32>> = prof.iter().map(|p| p.2.clone()).collect();
        let pc: Vec<Vec2> = prof.iter().map(|p| p.1).collect();
        for k in 1..prof.len() {
            let up = k - 1;
            let dn = (k + 1).min(prof.len() - 1);
            prof[k].1 = (pc[up] + pc[k] * 2.0 + pc[dn]) / 4.0;
            for j in 0..segs {
                prof[k].2[j] = (prev[up][j] + 2.0 * prev[k][j] + prev[dn][j]) / 4.0;
            }
        }
    }
    let mut pos = Vec::new();
    let nring = prof.len();
    for (k, (y, c, r)) in prof.iter().enumerate() {
        let t = k as f32 / (nring - 1).max(1) as f32;
        let fl = flare * t.powf(1.5);
        for (j, rj) in r.iter().enumerate() {
            let a = j as f32 / segs as f32 * std::f32::consts::TAU;
            let rr = rj + offset + fl;
            pos.push(Vec3::new(c.x + a.cos() * rr, *y, c.y + a.sin() * rr));
        }
    }
    let mut tris = Vec::new();
    for k in 0..nring - 1 {
        for j in 0..segs {
            let a = (k * segs + j) as u32;
            let b = (k * segs + (j + 1) % segs) as u32;
            let c = a + segs as u32;
            let d = b + segs as u32;
            // Outward: angle runs x -> z (counter-clockwise seen from -y).
            tris.push([a, c, b]);
            tris.push([b, c, d]);
        }
    }
    // Pick the winding that faces out.
    let n = vertex_normals(&pos, &tris);
    let c0 = prof[0].1;
    if (pos[0] - Vec3::new(c0.x, prof[0].0, c0.y)).dot(n[0]) < 0.0 {
        for t in &mut tris {
            t.swap(1, 2);
        }
    }
    Ok((pos, tris, prof.first().map_or(top, |p| p.0)))
}

fn fabric(cols: [&str; 2], rough: f32, name: &str) -> crate::material::PbrRef {
    use serde_json::json;
    crate::material::PbrRef {
        material: format!("garment_{name}"),
        params: Default::default(),
        scale: 1.0,
        graph: Some(Arc::new(crate::material::MatGraph {
            domain: "tile".into(),
            tile: 0.1,
            resolution: 256,
            nodes: vec![
                json!({"type": "noise", "name": "weave", "size": 0.0025, "octaves": 2, "seed": 3}),
                json!({"type": "noise", "name": "mottle", "size": 0.05, "octaves": 3, "seed": 9}),
                json!({"type": "math", "name": "mix", "op": "add", "a": "weave", "b": "mottle"}),
                json!({"type": "remap", "name": "t", "input": "mix", "from": [0.4, 1.6], "to": [0, 1]}),
                json!({"type": "ramp", "name": "col", "input": "t", "stops": [[0, cols[0]], [1, cols[1]]]}),
                json!({"type": "math", "name": "h", "op": "mul", "a": "weave", "b": 0.0004}),
                json!({"type": "output", "name": "o", "base_color": "col", "roughness": rough, "height": "h"}),
            ],
            params: Default::default(),
        })),
    }
}

/// Builds the garment from the part it is `on` (world placement baked in).
pub fn build(name: &str, def: &GarmentDef, body: &Part) -> Result<(Part, Vec<u32>), String> {
    let (d, cols, rough) = resolved(def)?;
    if body.verts.is_empty() || body.indices.is_empty() {
        return Err(format!("on: `{}` has no surface", body.name));
    }
    let offset = d.offset.unwrap_or(0.006);
    let thickness = d.thickness.unwrap_or(0.003);
    let ease = d.ease.unwrap_or(0.0);
    let w = weld(body);
    let mut mesh = Mesh::default();
    let mut covers: Vec<u32> = Vec::new();
    let mut region = d.region.clone();
    if let (Some(p), Some(r)) = (def.preset.as_deref(), region.as_mut())
        && p.starts_with("hair_")
        && r.planes.is_empty()
    {
        let h = body
            .joint("head")
            .ok_or_else(|| format!("`{}` has no `head` joint for the hairline", body.name))?;
        let s = hairline(body, &w, h);
        // Front hairline: above the brow, sloping down to the nape; the
        // ears and the face stay clear.
        let drop = if p == "hair_bob" { 0.06 } else { 0.0 };
        r.planes = vec![
            Plane {
                normal: Vec3::new(0.0, 1.0, -0.55).normalize().to_array(),
                point: [0.0, h.y + s * 0.42 - drop, h.z + s * 0.25],
            },
            Plane {
                normal: [0.0, 1.0, 0.0],
                point: [0.0, h.y + s * 0.12 - drop * 1.5, 0.0],
            },
        ];
    }
    if let Some(r) = &region {
        let s = field(body, &w, r)?;
        let (sh, cand) = clip(&w, &s);
        if sh.tris.is_empty() {
            return Err(format!(
                "region covers nothing of `{}` (check zones / heights: the body spans y {:.2}..{:.2})",
                body.name,
                body.bounds().0.y,
                body.bounds().1.y
            ));
        }
        let (p, dist) = shape_sheet(&sh, offset, ease, d.hem.as_ref());
        // Covered body faces: every corner well inside the garment.
        let mut at: HashMap<u32, f32> = HashMap::new();
        for (i, o) in sh.orig.iter().enumerate() {
            if let Some(g) = o {
                at.insert(*g, dist[i]);
            }
        }
        for ti in cand {
            let t = w.tris[ti as usize];
            if t.iter().all(|g| at.get(g).is_some_and(|x| *x >= 0.012)) {
                covers.push(w.raw_tri[ti as usize]);
            }
        }
        let mut p = p;
        if def.preset.as_deref() == Some("shoes") {
            convex_slices(&mut p, &sh.tris);
        }
        emit(&mut mesh, &p, &sh.tris, thickness, Some(&sh.src));
        // Shoes: a flat sole under each foot.
        if def.preset.as_deref() == Some("shoes") {
            for side in [1.0f32, -1.0] {
                let pts: Vec<Vec2> = sh
                    .base
                    .iter()
                    .filter(|q| q.x * side > 0.0 && q.y < 0.05)
                    .map(|q| Vec2::new(q.x, q.z))
                    .collect();
                let h = hull2(pts);
                if h.len() < 3 {
                    continue;
                }
                sole(&mut mesh, &h, offset + thickness + 0.002, 0.024);
            }
        }
    }
    if let Some(sk) = &d.skirt {
        let top = level(&sk.top, body)?;
        let hem = level(&sk.hem, body)?;
        // Outside the top it is tucked over (a tank, a dress bodice).
        let (p, t, _) = skirt_tube(body, &w, top, hem, sk.flare.unwrap_or(0.08), offset + 0.008)?;
        emit(&mut mesh, &p, &t, thickness, None);
        // Hide the body between the waistband and the hem (legs included:
        // they are inside the skirt).
        let lo = hem + 0.03;
        let hi = top - 0.02;
        let arm: Vec<bool> = arm_mask(body, &w);
        for (ti, t) in w.tris.iter().enumerate() {
            if t.iter().all(|g| {
                let q = w.pos[*g as usize];
                q.y > lo && q.y < hi && !arm[*g as usize]
            }) {
                covers.push(w.raw_tri[ti]);
            }
        }
    }
    covers.sort_unstable();
    covers.dedup();
    let mut part = Part::new(name);
    part.verts = mesh.pos.iter().map(|v| v.to_array()).collect();
    part.indices = mesh.tris.iter().flatten().copied().collect();
    part.normals = vertex_normals(&mesh.pos, &mesh.tris)
        .iter()
        .map(|v| v.to_array())
        .collect();
    // Cylindrical metric uvs about the vertical axis through the piece.
    let (lo, hi) = mesh
        .pos
        .iter()
        .fold((Vec3::MAX, Vec3::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
    let c = (lo + hi) * 0.5;
    let rad = ((hi.x - lo.x).max(hi.z - lo.z) * 0.5).max(0.05);
    part.uvs = mesh
        .pos
        .iter()
        .map(|v| {
            let a = (v.z - c.z).atan2(v.x - c.x);
            [a * rad, -v.y]
        })
        .collect();
    part.sheet = thickness <= 0.0;
    part.skin = carried_skin(body, &w, &mut mesh, def.preset.as_deref() == Some("shoes"));
    part.material.base_color = [1.0; 4];
    part.material.roughness = 1.0;
    part.material.metalness = 0.0;
    part.material.pbr = Some(fabric(
        cols,
        rough,
        def.preset.as_deref().unwrap_or("cloth"),
    ));
    if d.hide_skin == Some(false) {
        covers.clear();
    }
    Ok((part, covers))
}

fn arm_mask(body: &Part, w: &Welded) -> Vec<bool> {
    let Some(sk) = &body.skin else {
        return vec![false; w.pos.len()];
    };
    let bz: Vec<Zone> = sk.bones.iter().map(|b| zone_of_bone(b)).collect();
    (0..w.pos.len())
        .map(|g| {
            let raw = w.first[g] as usize;
            let j = sk.joints.get(raw).copied().unwrap_or_default();
            let wt = sk.weights.get(raw).copied().unwrap_or_default();
            (0..4)
                .filter(|k| matches!(bz.get(j[*k] as usize), Some(Zone::Arm | Zone::Hand)))
                .map(|k| wt[k])
                .sum::<f32>()
                > 0.2
        })
        .collect()
}

/// Pushes every vertex out to the convex hull of its horizontal slice, per
/// side of x = 0 (a shoe bridges the toes and the arch).
fn convex_slices(p: &mut [Vec3], tris: &[[u32; 3]]) {
    let nrm = vertex_normals(p, tris);
    let bin = 0.008;
    let mut slices: HashMap<(i32, bool), Vec<usize>> = HashMap::new();
    for (i, v) in p.iter().enumerate() {
        slices
            .entry(((v.y / bin).floor() as i32, v.x >= 0.0))
            .or_default()
            .push(i);
    }
    let orig: Vec<Vec3> = p.to_vec();
    for ((k, side), idx) in &slices {
        // Hull over this slice and its neighbours, so the slices agree.
        let mut pts = Vec::new();
        for dk in -1..=1 {
            if let Some(n) = slices.get(&(k + dk, *side)) {
                pts.extend(n.iter().map(|i| Vec2::new(orig[*i].x, orig[*i].z)));
            }
        }
        let h = hull2(pts);
        if h.len() < 3 {
            continue;
        }
        let c = h.iter().copied().sum::<Vec2>() / h.len() as f32;
        for &i in idx {
            // Side walls only: a top or sole face would slide to the rim.
            if nrm[i].y.abs() > 0.7 {
                continue;
            }
            let q = Vec2::new(orig[i].x, orig[i].z) - c;
            let r = q.length();
            if r < 1e-6 {
                continue;
            }
            let a = q.y.atan2(q.x);
            let rh = ray_hull(&h, c, a);
            if rh > r {
                let o = c + q / r * rh.min(r + 0.025);
                p[i].x = o.x;
                p[i].z = o.y;
            }
        }
    }
}

/// The body's own skin weights at every garment vertex (exact, from the
/// vertex it was offset from; a sole takes its nearest upper vertex's),
/// so a smooth bind carries them instead of a closest-point transfer
/// (which mixes a sleeve hem near the torso into the chest). `None`
/// without body weights or when a vertex has no source (a skirt tube).
fn carried_skin(
    body: &Part,
    w: &Welded,
    mesh: &mut Mesh,
    stiff_toes: bool,
) -> Option<Arc<crate::human::SkinWeights>> {
    let sk = body.skin.as_ref()?;
    // A shoe doesn't fold at the toes: ball / toe weights go to the ankle.
    let remap: Vec<u16> = sk
        .bones
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let side = b.rsplit('_').next().unwrap_or("");
            let ankle = format!("ankle_{side}");
            match (stiff_toes, b.starts_with("ball_") || b.starts_with("toe_")) {
                (true, true) => sk
                    .bones
                    .iter()
                    .position(|x| *x == ankle)
                    .map_or(i as u16, |a| a as u16),
                _ => i as u16,
            }
        })
        .collect();
    // Fill sourceless vertices (soles) from the nearest sourced one.
    let have: Vec<usize> = (0..mesh.pos.len())
        .filter(|i| mesh.src[*i].is_some())
        .collect();
    if have.is_empty() {
        return None;
    }
    for i in 0..mesh.pos.len() {
        if mesh.src[i].is_some() {
            continue;
        }
        let p = mesh.pos[i];
        let near = have
            .iter()
            .min_by(|a, b| {
                let xz = |q: Vec3| Vec2::new(q.x - p.x, q.z - p.z).length_squared();
                // Plan distance: a sole's top and bottom take the same source.
                xz(mesh.pos[**a]).total_cmp(&xz(mesh.pos[**b]))
            })
            .copied()?;
        if Vec2::new(mesh.pos[near].x - p.x, mesh.pos[near].z - p.z).length() > 0.08 {
            return None;
        }
        mesh.src[i] = mesh.src[near];
    }
    let raw = |g: u32| w.first[g as usize] as usize;
    let mut joints = Vec::with_capacity(mesh.pos.len());
    let mut weights = Vec::with_capacity(mesh.pos.len());
    for s in &mesh.src {
        let (a, b, t) = (*s)?;
        let mut acc: Vec<(u16, f32)> = Vec::with_capacity(8);
        for (g, f) in [(a, 1.0 - t), (b, t)] {
            let (j, wt) = (
                sk.joints.get(raw(g)).copied().unwrap_or_default(),
                sk.weights.get(raw(g)).copied().unwrap_or_default(),
            );
            for k in 0..4 {
                let jk = remap.get(j[k] as usize).copied().unwrap_or(j[k]);
                if wt[k] * f > 0.0 {
                    match acc.iter_mut().find(|x| x.0 == jk) {
                        Some(x) => x.1 += wt[k] * f,
                        None => acc.push((jk, wt[k] * f)),
                    }
                }
            }
        }
        acc.sort_by(|x, y| y.1.total_cmp(&x.1));
        acc.truncate(4);
        let sum: f32 = acc.iter().map(|x| x.1).sum::<f32>().max(1e-9);
        let mut jj = [0u16; 4];
        let mut ww = [0f32; 4];
        for (k, (j, x)) in acc.iter().enumerate() {
            jj[k] = *j;
            ww[k] = x / sum;
        }
        joints.push(jj);
        weights.push(ww);
    }
    // Diffuse the weights a little over the garment's own edges: cloth
    // bridging a fold (the armpit) links vertices the body keeps apart.
    let (nb, _) = neighbours(mesh.pos.len(), &mesh.tris);
    for _ in 0..6 {
        let pj = joints.clone();
        let pw = weights.clone();
        for i in 0..mesh.pos.len() {
            if nb[i].is_empty() {
                continue;
            }
            let mut acc: Vec<(u16, f32)> = Vec::with_capacity(16);
            let mut add = |j: &[u16; 4], w: &[f32; 4], f: f32| {
                for k in 0..4 {
                    if w[k] > 0.0 {
                        match acc.iter_mut().find(|x| x.0 == j[k]) {
                            Some(x) => x.1 += w[k] * f,
                            None => acc.push((j[k], w[k] * f)),
                        }
                    }
                }
            };
            add(&pj[i], &pw[i], 0.5);
            let f = 0.5 / nb[i].len() as f32;
            for &n in &nb[i] {
                add(&pj[n as usize], &pw[n as usize], f);
            }
            acc.sort_by(|x, y| y.1.total_cmp(&x.1));
            acc.truncate(4);
            let sum: f32 = acc.iter().map(|x| x.1).sum::<f32>().max(1e-9);
            let mut jj = [0u16; 4];
            let mut ww = [0f32; 4];
            for (k, (j, x)) in acc.iter().enumerate() {
                jj[k] = *j;
                ww[k] = x / sum;
            }
            joints[i] = jj;
            weights[i] = ww;
        }
    }
    Some(Arc::new(crate::human::SkinWeights {
        bones: sk.bones.clone(),
        joints,
        weights,
    }))
}

/// The head's height above its joint (crown y - joint y).
fn hairline(_body: &Part, w: &Welded, h: Vec3) -> f32 {
    let top = w
        .pos
        .iter()
        .filter(|p| (p.x - h.x).abs() < 0.12 && (p.z - h.z).abs() < 0.15)
        .map(|p| p.y)
        .fold(h.y, f32::max);
    (top - h.y).max(0.05)
}

/// A flat sole: the footprint hull grown by `grow`, from y = 0 to `h`.
fn sole(mesh: &mut Mesh, hull: &[Vec2], grow: f32, h: f32) {
    let c = hull.iter().copied().sum::<Vec2>() / hull.len() as f32;
    let ring: Vec<Vec2> = hull
        .iter()
        .map(|p| *p + (*p - c).normalize_or_zero() * grow)
        .collect();
    let base = mesh.pos.len() as u32;
    let n = ring.len() as u32;
    for p in &ring {
        mesh.pos.push(Vec3::new(p.x, 0.0, p.y));
    }
    for p in &ring {
        mesh.pos.push(Vec3::new(p.x, h, p.y));
    }
    let cb = mesh.pos.len() as u32;
    mesh.pos.push(Vec3::new(c.x, 0.0, c.y));
    mesh.pos.push(Vec3::new(c.x, h, c.y));
    mesh.src
        .extend(std::iter::repeat_n(None, 2 * ring.len() + 2));
    // Hull is counter-clockwise in (x, z): seen from +y that is clockwise.
    for k in 0..n {
        let (a, b) = (base + k, base + (k + 1) % n);
        let (at, bt) = (a + n, b + n);
        mesh.tris.push([a, at, b]);
        mesh.tris.push([b, at, bt]);
        mesh.tris.push([cb + 1, bt, at]);
        mesh.tris.push([cb, a, b]);
    }
    // Make sure the side faces point out.
    let [a, b, cc] = mesh.tris[mesh.tris.len() - 4].map(|i| mesh.pos[i as usize]);
    let out = (a - Vec3::new(c.x, a.y, c.y)).normalize_or_zero();
    if (b - a).cross(cc - a).dot(out) < 0.0 {
        let k = mesh.tris.len() - 4 * n as usize;
        for t in &mut mesh.tris[k..] {
            t.swap(1, 2);
        }
    }
}

type CoverMemo = HashMap<u64, Arc<Vec<u32>>>;

fn cover_memo() -> &'static std::sync::Mutex<CoverMemo> {
    static M: std::sync::OnceLock<std::sync::Mutex<CoverMemo>> = std::sync::OnceLock::new();
    M.get_or_init(Default::default)
}

/// The memo key of a garment op over a body surface.
fn cover_key(op: &crate::Op, body: &Part) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(op).unwrap_or_default().hash(&mut h);
    body.indices.len().hash(&mut h);
    for v in &body.verts {
        v.map(f32::to_bits).hash(&mut h);
    }
    for v in [body.translation, body.scale] {
        v.map(f32::to_bits).hash(&mut h);
    }
    body.rotation.map(f32::to_bits).hash(&mut h);
    h.finish()
}

/// Records the body faces a garment op covers (from [`build`]).
pub fn remember(op: &crate::Op, body: &Part, covers: Vec<u32>) {
    let k = cover_key(op, body);
    if let Ok(mut m) = cover_memo().lock() {
        if m.len() > 512 {
            m.clear();
        }
        m.insert(k, Arc::new(covers));
    }
}

/// Drops from every body the faces the visible garments on it cover
/// (vertices kept, so weights, morphs and correctives keep their counts).
/// Runs at the end of graph evaluation, before materials map the parts.
pub fn mask_covered(ops: &[crate::Op], named: &mut [(String, Arc<Part>)]) {
    let mut want: HashMap<String, Vec<u32>> = HashMap::new();
    for op in ops {
        let crate::Op::Garment { name, def } = op else {
            continue;
        };
        if def.hide_skin == Some(false) {
            continue;
        }
        let Some((_, g)) = named.iter().find(|(n, _)| n == name) else {
            continue;
        };
        if !g.visible {
            continue;
        }
        let Some((_, body)) = named.iter().find(|(n, _)| *n == def.on) else {
            continue;
        };
        let k = cover_key(op, body);
        let hit = cover_memo().lock().ok().and_then(|m| m.get(&k).cloned());
        let covers = match hit {
            Some(c) => c,
            None => match build(name, def, body) {
                Ok((_, c)) => {
                    remember(op, body, c.clone());
                    Arc::new(c)
                }
                Err(_) => continue,
            },
        };
        want.entry(def.on.clone())
            .or_default()
            .extend_from_slice(&covers);
    }
    for (n, p) in named.iter_mut() {
        let Some(drop) = want.get_mut(n.as_str()) else {
            continue;
        };
        if drop.is_empty() {
            continue;
        }
        drop.sort_unstable();
        drop.dedup();
        let pm = Arc::make_mut(p);
        let mut out = Vec::with_capacity(pm.indices.len());
        for (t, tri) in pm.indices.as_chunks::<3>().0.iter().enumerate() {
            if drop.binary_search(&(t as u32)).is_err() {
                out.extend_from_slice(tri);
            }
        }
        pm.indices = out;
        // An open hull on purpose now (validate skips sheets).
        pm.sheet = true;
    }
}

/// Parts whose names match none of `show` and match `hide`, for variants.
pub fn set_visible(parts: &mut [Arc<Part>], show: &[String], hide: &[String]) -> Vec<String> {
    let mut changed = Vec::new();
    for p in parts.iter_mut() {
        let s = show.iter().any(|g| name_matches(g, &p.name));
        let h = hide.iter().any(|g| name_matches(g, &p.name));
        let want = if s {
            true
        } else if h {
            false
        } else {
            continue;
        };
        if p.visible != want {
            Arc::make_mut(p).visible = want;
            changed.push(p.name.clone());
        }
    }
    changed
}

#[cfg(test)]
mod tests;
