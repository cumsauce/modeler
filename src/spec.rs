//! The `apply_ops` wire format: one JSON object per item, tagged by "type".
//! Items are parsed one at a time so errors name the item and field
//! (`op #3 (primitive "roof"): size: ...`), and the lenient forms (sizes as a
//! number or {radius, height}, RGB/hex colours, ...) are resolved here.
//! Values may reference model parameters (`"$tower_h"`, `"=$tower_h / 2"`);
//! see [`crate::params`].

use crate::commands::BatchItem;
use crate::{Axis, BoolOp, Op, PrimitiveKind};
use serde::de::DeserializeOwned;
use serde_json::Value;

// ---------------------------------------------------------------------------
// Lenient value forms
// ---------------------------------------------------------------------------

/// A primitive size in any of the accepted forms, resolved against the kind
/// by [`resolve_size`]: a number (sphere/disc radius, box/wedge cube edge),
/// an array of 1..3 numbers (padded), or an object with named fields
/// ({radius, height}, {ring, tube}, {inner, outer}, {x, y, z}).
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(transparent)]
pub struct SizeArg(pub Value);

impl schemars::JsonSchema for SizeArg {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Size".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "Primitive size. Any of: a number (sphere/disc radius, box/wedge cube edge); [a, b, c] with trailing values optional (box/plane/wedge full extents; [radius] sphere/disc; [radius, height] cylinder/cone/capsule; [ring, tube] torus; [inner, outer] washer); or an object {radius, height} / {ring, tube} / {inner, outer} / {x, y, z}.",
            "anyOf": [
                {"type": "number"},
                {"type": "array", "items": {"type": "number"}, "minItems": 1, "maxItems": 3},
                {"type": "object"}
            ]
        })
    }
}

fn num(v: &Value, what: &str) -> Result<f32, String> {
    v.as_f64()
        .map(|f| f as f32)
        .ok_or_else(|| format!("{what} must be a number, got {v}"))
}

/// Turns a lenient size into the graph's `[f32; 3]` for `kind`.
pub fn resolve_size(kind: PrimitiveKind, v: &Value) -> Result<[f32; 3], String> {
    use PrimitiveKind as K;
    let need = match kind {
        K::Sphere | K::Disc => 1,
        K::Cylinder | K::Cone | K::Capsule | K::Torus | K::Washer | K::Plane => 2,
        K::Box | K::Wedge => 3,
    };
    let names: &[&str] = match kind {
        K::Sphere | K::Disc => &["radius"],
        K::Cylinder | K::Cone | K::Capsule => &["radius", "height"],
        K::Torus => &["ring", "tube"],
        K::Washer => &["inner", "outer"],
        K::Plane => &["x", "y"],
        K::Box | K::Wedge => &["x", "y", "z"],
    };
    let boxy = matches!(kind, K::Box | K::Wedge | K::Plane);
    let expected = format!(
        "{kind} size is [{}] (or {{{}}})",
        names.join(", "),
        names.join(", ")
    );
    match v {
        Value::Number(_) => {
            let n = num(v, "size")?;
            match kind {
                K::Sphere | K::Disc => Ok([n, 0.0, 0.0]),
                K::Box | K::Wedge => Ok([n, n, n]),
                K::Plane => Ok([n, n, 0.0]),
                _ => Err(format!("a single number is ambiguous here: {expected}")),
            }
        }
        Value::Array(items) => {
            if items.is_empty() || items.len() > 3 {
                return Err(format!("{expected}; got {} elements", items.len()));
            }
            let vals: Vec<f32> = items
                .iter()
                .enumerate()
                .map(|(i, x)| num(x, &format!("size[{i}]")))
                .collect::<Result<_, _>>()?;
            // A one-element box/plane is a cube/square.
            if vals.len() == 1 && boxy {
                return resolve_size(kind, &items[0]);
            }
            if vals.len() < need {
                return Err(format!("{expected}; got {} elements", vals.len()));
            }
            let mut out = [0.0; 3];
            out[..vals.len()].copy_from_slice(&vals);
            Ok(out)
        }
        Value::Object(map) => {
            // Box/plane also accept width/height/depth.
            let alias = |k: &str| match k {
                "x" => "width",
                "y" => "height",
                "z" => "depth",
                _ => "",
            };
            let mut out = [0.0; 3];
            for (i, k) in names.iter().enumerate() {
                let got = map.get(*k).or_else(|| map.get(alias(k)).filter(|_| boxy));
                match got {
                    Some(x) => out[i] = num(x, &format!("size.{k}"))?,
                    None => return Err(format!("size is missing `{k}`: {expected}")),
                }
            }
            if let Some(extra) = map.keys().find(|k| {
                !names.contains(&k.as_str())
                    && !(boxy && ["width", "height", "depth"].contains(&k.as_str()))
            }) {
                return Err(format!("size has unknown field `{extra}`: {expected}"));
            }
            Ok(out)
        }
        other => Err(format!("{expected}; got {other}")),
    }
}

/// A colour: RGBA [r, g, b, a], RGB [r, g, b] (alpha 1), components 0..1 or
/// 0..255, or a hex string "#rgb", "#rrggbb" or "#rrggbbaa".
#[derive(Clone, Copy, Debug)]
pub struct ColorArg(pub [f32; 4]);

impl<'de> serde::Deserialize<'de> for ColorArg {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        parse_color(&v)
            .map(ColorArg)
            .map_err(serde::de::Error::custom)
    }
}

impl schemars::JsonSchema for ColorArg {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Color".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "Colour: [r, g, b] or [r, g, b, a] (0..1, or 0..255), or \"#rgb\", \"#rrggbb\" or \"#rrggbbaa\".",
            "anyOf": [
                {"type": "array", "items": {"type": "number"}, "minItems": 3, "maxItems": 4},
                {"type": "string"}
            ]
        })
    }
}

/// A colour value as RGBA 0..1 (for style tokens).
pub fn parse_color_value(v: &Value) -> Result<[f32; 4], String> {
    parse_color(v)
}

fn parse_color(v: &Value) -> Result<[f32; 4], String> {
    match v {
        Value::Array(items) if items.len() == 3 || items.len() == 4 => {
            let mut c = [1.0f32; 4];
            for (i, x) in items.iter().enumerate() {
                c[i] = num(x, &format!("color[{i}]"))?;
            }
            if c.iter().any(|x| *x > 1.0) {
                for x in &mut c {
                    *x /= 255.0;
                }
                if items.len() == 3 {
                    c[3] = 1.0;
                }
            }
            Ok(c)
        }
        Value::Array(items) => Err(format!(
            "colour needs 3 (RGB) or 4 (RGBA) components, got {}",
            items.len()
        )),
        Value::String(s) => {
            let h = s.trim_start_matches('#');
            // CSS short forms #rgb / #rgba expand each digit (#222 = #222222).
            let long: String;
            let h = if h.len() == 3 || h.len() == 4 {
                long = h.chars().flat_map(|c| [c, c]).collect();
                long.as_str()
            } else {
                h
            };
            if !(h.len() == 6 || h.len() == 8) || !h.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(format!(
                    "colour string must be #rgb, #rrggbb or #rrggbbaa, got {s:?}"
                ));
            }
            let byte =
                |i: usize| u8::from_str_radix(&h[i..i + 2], 16).unwrap_or(255) as f32 / 255.0;
            Ok([
                byte(0),
                byte(2),
                byte(4),
                if h.len() == 8 { byte(6) } else { 1.0 },
            ])
        }
        other => Err(format!(
            "colour must be [r, g, b(, a)] or \"#rrggbb\", got {other}"
        )),
    }
}

// ---------------------------------------------------------------------------
// apply_ops items
// ---------------------------------------------------------------------------

fn yes() -> bool {
    true
}

pub fn default_axis() -> Axis {
    Axis::Y
}

pub fn default_tool_degrees() -> f32 {
    360.0
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PrimitiveSpec {
    /// Unique output part name; re-using a name replaces that op (parameter tweak).
    name: String,
    /// box, sphere, cylinder, cone, capsule, torus, plane, wedge, disc, washer.
    kind: PrimitiveKind,
    size: SizeArg,
    /// Position [x, y, z]; default origin. Alias: "position".
    #[serde(default, alias = "position")]
    translation: [f32; 3],
    /// Rotation: [x, y, z, w], {"axis": [1, 0, 0], "degrees": -20} or {"x": -20} (degrees).
    #[serde(default)]
    rotation: Option<RotationSpec>,
    /// Build in another part's own coordinates: "paper" or "paper:top-front"
    /// (an anchor on its local box: left/right, bottom/top, back/front,
    /// center). translation and rotation are then relative to that part, and
    /// this primitive follows it when it moves or turns.
    #[serde(default)]
    frame: Option<String>,
}

/// One entry of a profile ring: a point [a, b], or an arc that expands to
/// points: {"arc": {"center": [a, b], "radius": r, "from": deg, "to": deg}}
/// (degrees counter-clockwise from +a; `segments` optional).
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum RingItem {
    Point([f32; 2]),
    Arc { arc: ArcSpec },
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ArcSpec {
    center: [f32; 2],
    radius: f32,
    /// Start angle, degrees counter-clockwise from +a.
    from: f32,
    /// End angle; to > from runs counter-clockwise. A full circle is 0 -> 360.
    to: f32,
    /// Points along the arc; default one per ~7.5 degrees.
    #[serde(default)]
    segments: Option<u32>,
}

/// A 2D profile: one ring of points/arcs, or a list of rings (inner rings
/// cut holes; orientation doesn't matter).
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum ProfileArg {
    Ring(Vec<RingItem>),
    Rings(Vec<Vec<RingItem>>),
}

fn expand_ring(items: Vec<RingItem>) -> Result<Vec<[f32; 2]>, String> {
    let mut out = Vec::new();
    for it in items {
        match it {
            RingItem::Point(p) => out.push(p),
            RingItem::Arc { arc } => {
                if arc.radius.is_nan() || arc.radius <= 0.0 {
                    return Err(format!("arc radius must be > 0, got {}", arc.radius));
                }
                let sweep = arc.to - arc.from;
                let full = (sweep.abs() - 360.0).abs() < 1e-3;
                let n = arc
                    .segments
                    .unwrap_or_else(|| (sweep.abs() / 7.5).ceil().max(2.0) as u32)
                    .max(2);
                // A full circle repeats its start point; drop the duplicate.
                let last = if full { n - 1 } else { n };
                for k in 0..=last {
                    let a = (arc.from + sweep * k as f32 / n as f32).to_radians();
                    out.push([
                        arc.center[0] + arc.radius * a.cos(),
                        arc.center[1] + arc.radius * a.sin(),
                    ]);
                }
            }
        }
    }
    // Drop consecutive duplicates (an arc that starts where a point ended).
    out.dedup_by(|a, b| (a[0] - b[0]).abs() < 1e-7 && (a[1] - b[1]).abs() < 1e-7);
    if out.len() > 2 && {
        let (f, l) = (out[0], out[out.len() - 1]);
        (f[0] - l[0]).abs() < 1e-7 && (f[1] - l[1]).abs() < 1e-7
    } {
        out.pop();
    }
    Ok(out)
}

impl ProfileArg {
    fn rings(self) -> Result<Vec<Vec<[f32; 2]>>, String> {
        match self {
            ProfileArg::Ring(r) => Ok(vec![expand_ring(r)?]),
            ProfileArg::Rings(r) => r.into_iter().map(expand_ring).collect(),
        }
    }
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExtrudeSpec {
    name: String,
    /// Footprint on the ground: [[x, z], ...], or several rings (inner rings
    /// are holes). The solid spans y = 0..height before translation.
    profile: ProfileArg,
    height: f32,
    /// Degrees the top turns about the profile origin (counter-clockwise from above).
    #[serde(default)]
    twist: f32,
    /// Top ring scale [sx, sz] about the profile origin [0, 0]; [0, 0] tapers
    /// to a point there. Default [1, 1].
    #[serde(default = "one2")]
    scale_top: [f32; 2],
    /// Intermediate slices for twist; 0 picks automatically.
    #[serde(default)]
    slices: u32,
    /// Round every profile corner with this radius (meters). Default 0.
    #[serde(default)]
    round: f32,
    #[serde(default, alias = "position")]
    translation: [f32; 3],
    /// Rotation: [x, y, z, w], {"axis": [1, 0, 0], "degrees": -20} or {"x": -20} (degrees).
    #[serde(default)]
    rotation: Option<RotationSpec>,
}

fn one2() -> [f32; 2] {
    [1.0, 1.0]
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RevolveSpec {
    name: String,
    /// Half cross-section: [[radius, y], ...] with radius >= 0, revolved
    /// around the y axis (a vase, bowl, column, bottle). Several rings: inner
    /// ones are holes (a ring torus).
    profile: ProfileArg,
    /// Sweep angle, (0, 360]; under 360 leaves flat end caps.
    #[serde(default = "default_tool_degrees")]
    degrees: f32,
    /// Segments around the axis; 0 = 48.
    #[serde(default)]
    segments: u32,
    /// Round every profile corner with this radius (meters). Default 0.
    #[serde(default)]
    round: f32,
    #[serde(default, alias = "position")]
    translation: [f32; 3],
    /// Rotation: [x, y, z, w], {"axis": [1, 0, 0], "degrees": -20} or {"x": -20} (degrees).
    #[serde(default)]
    rotation: Option<RotationSpec>,
}

/// One rewrite rule: a string, or several choices picked at random per
/// occurrence (`["F[+F]F", "F[-F]F"]` equally, or `[{"p": 3, "to": ...}]`
/// weighted).
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum RuleSpec {
    One(String),
    Many(Vec<ChoiceSpec>),
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum ChoiceSpec {
    Plain(String),
    Weighted { p: f32, to: String },
}

/// An L-system: an axiom rewritten by rules `iterations` times, then drawn
/// by a 3D turtle. Wood goes to `name` (one tube per branch, tapered and
/// smooth at bends), leaves to `name_leaves`. Symbols: F/G draw, f move,
/// +/- turn, &/^ pitch, \\ and / roll, | turn around, [ ] branch (thinner by
/// radius_scale, shorter by length_scale), ! thin, L leaf; other letters
/// only rewrite. Start from a `preset` and override any field.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LSystemSpec {
    name: String,
    /// "tree", "pine", "bush", "fern", "weed" or "roots": axiom, rules and
    /// sizes for that shape; every other field overrides it.
    #[serde(default)]
    preset: Option<String>,
    /// The starting string, e.g. "X".
    #[serde(default)]
    axiom: Option<String>,
    /// Symbol -> replacement: {"X": "F[+X][-X]FX", "F": "FF"}; a list of
    /// choices is stochastic: {"X": ["F[+X]FX", {"p": 2, "to": "F[-X]X"}]}.
    /// Parametric: modules carry numbers, `A(1, 0.1)`; a head names them
    /// and may test them, {"A(l, w) : l > 0.05": "!(w)F(l)[&(40)A(l*0.6, w*0.7)]/(137.5)A(l*0.9, w*0.8)"}
    /// (+ - * / % ^, comparisons, && || !, min max abs sqrt sin cos floor;
    /// the turtle reads F(len) f(len) +(deg) !(radius) L(scale)).
    #[serde(default)]
    rules: Option<serde_json::Map<String, serde_json::Value>>,
    /// Named numbers for rule expressions: {"r1": 0.9, "a": 40} (may be "$param").
    #[serde(default)]
    constants: Option<std::collections::BTreeMap<String, f64>>,
    /// Rewrites (0..12; the string grows exponentially).
    #[serde(default)]
    iterations: Option<u32>,
    /// Turn angle, degrees (25).
    #[serde(default)]
    angle: Option<f32>,
    /// Segment length at the trunk, meters.
    #[serde(default)]
    length: Option<f32>,
    /// Segment length multiplier per branch level (0.9).
    #[serde(default)]
    length_scale: Option<f32>,
    /// Trunk radius, meters.
    #[serde(default)]
    radius: Option<f32>,
    /// Radius multiplier per branch level and per `!` (0.7).
    #[serde(default)]
    radius_scale: Option<f32>,
    /// Radius multiplier per segment along a branch (0.97).
    #[serde(default)]
    taper: Option<f32>,
    /// Roll at every branch, degrees (137.5 spreads them round the trunk).
    #[serde(default)]
    branch_roll: Option<f32>,
    /// Random +- degrees on every turn (natural variation).
    #[serde(default)]
    jitter: Option<f32>,
    /// Random +- share of every segment length (0.1 = 10%).
    #[serde(default)]
    length_jitter: Option<f32>,
    /// Direction branches bend toward: [0, -1, 0] droops (willow, fruit
    /// load), [0, 1, 0] reaches up (poplar).
    #[serde(default)]
    tropism: Option<[f32; 3]>,
    /// How hard they bend per segment (0.05 gentle, 0.3 strong).
    #[serde(default)]
    tropism_strength: Option<f32>,
    /// Random seed: the same seed grows the same plant; vary it for a forest.
    #[serde(default)]
    seed: Option<u64>,
    /// Sides of each branch tube (3..32, 7).
    #[serde(default)]
    sides: Option<u32>,
    /// Leaf blade [width, length], meters.
    #[serde(default)]
    leaf_size: Option<[f32; 2]>,
    /// Leaf outline: ellipse (default), lance, round, heart, diamond, needle, maple.
    #[serde(default)]
    leaf_shape: Option<String>,
    /// Leaves pitch this many degrees out from their branch.
    #[serde(default)]
    leaf_angle: Option<f32>,
    /// Leaves bend this many degrees from base to tip (drooping tips).
    #[serde(default)]
    leaf_curl: Option<f32>,
    /// V-fold along the midrib, degrees.
    #[serde(default)]
    leaf_fold: Option<f32>,
    /// Leaf stalk (petiole) length, meters.
    #[serde(default)]
    leaf_stalk: Option<f32>,
    /// "card" (default): each leaf is a flat quad with uvs 0..1, for an
    /// alpha-cutout leaf texture (a material with domain "card" whose
    /// output sets opacity): a few triangles a leaf, the way games draw
    /// foliage. "solid": a closed blade modelled in its outline shape
    /// (needs no texture, ~60 triangles a leaf).
    #[serde(default)]
    leaf_mode: Option<String>,
    /// Card leaves: add a second card crossed at 90 degrees, so each leaf
    /// has volume from every angle.
    #[serde(default)]
    leaf_cross: Option<bool>,
    #[serde(default, alias = "position")]
    translation: Option<[f32; 3]>,
    /// Rotation: [x, y, z, w], {"axis": [1, 0, 0], "degrees": -20} or {"x": -20} (degrees).
    #[serde(default)]
    rotation: Option<RotationSpec>,
}

/// Axiom, rules and sizes per preset (meters).
pub(crate) fn lsystem_preset(name: &str) -> Result<crate::lsystem::LSystem, String> {
    use crate::lsystem::{Choice, LSystem};
    let r = |pairs: &[(char, &[(f32, &str)])]| {
        pairs
            .iter()
            .map(|(k, cs)| {
                (
                    k.to_string(),
                    cs.iter()
                        .map(|(p, to)| Choice {
                            p: *p,
                            to: (*to).into(),
                        })
                        .collect(),
                )
            })
            .collect()
    };
    let base = LSystem {
        axiom: "X".into(),
        rules: Default::default(),
        constants: Default::default(),
        iterations: 4,
        angle: 25.0,
        length: 0.3,
        length_scale: 0.9,
        radius: 0.08,
        radius_scale: 0.7,
        taper: 0.97,
        branch_roll: 137.5,
        jitter: 6.0,
        length_jitter: 0.12,
        tropism: [0.0, -1.0, 0.0],
        tropism_strength: 0.03,
        seed: 1,
        sides: 7,
        leaf_size: [0.08, 0.14],
        leaf: crate::lsystem::LeafStyle {
            shape: "ellipse".into(),
            angle: 35.0,
            curl: 25.0,
            fold: 25.0,
            stalk: 0.0,
            ..Default::default()
        },
        translation: [0.0; 3],
        rotation: None,
    };
    let leaf =
        |shape: &str, angle: f32, curl: f32, fold: f32, stalk: f32| crate::lsystem::LeafStyle {
            shape: shape.into(),
            angle,
            curl,
            fold,
            stalk,
            ..Default::default()
        };
    Ok(match name {
        // A broadleaf: a trunk, then forks spreading around it (about 4-5 m).
        "tree" => LSystem {
            axiom: "FFFA".into(),
            rules: r(&[
                (
                    'A',
                    &[
                        (1.0, "!F[&&BL]/////[&&BL]///////[&BL]A"),
                        (1.0, "!F[&&BL]///////[&BL]A"),
                    ],
                ),
                (
                    'B',
                    &[(1.0, "F[+BLL][-BLL]FBLL"), (1.0, "F[&BLL]F[^BLL]BLL")],
                ),
            ]),
            iterations: 5,
            angle: 28.0,
            length: 0.45,
            length_scale: 0.75,
            radius: 0.12,
            radius_scale: 0.72,
            leaf_size: [0.16, 0.26],
            leaf: leaf("ellipse", 40.0, 30.0, 25.0, 0.03),
            ..base
        },
        // A conifer: a straight leader, whorls of drooping branches that
        // shorten toward the top.
        "pine" => LSystem {
            axiom: "FFA[LLL]".into(),
            rules: r(&[
                (
                    'A',
                    &[(1.0, "F[&&&B]//////[&&&B]//////[&&&B]//////[&&&B]A")],
                ),
                ('B', &[(1.0, "F[+LL][-LL]&LBL")]),
            ]),
            iterations: 7,
            angle: 22.0,
            length: 0.35,
            length_scale: 0.62,
            radius: 0.12,
            radius_scale: 0.3,
            taper: 0.86,
            branch_roll: 0.0,
            tropism_strength: 0.06,
            leaf_size: [0.012, 0.22],
            leaf: leaf("needle", 25.0, 10.0, 0.0, 0.0),
            ..base
        },
        // A low shrub: forks from the ground, dense leaves (about 1 m).
        "bush" => LSystem {
            axiom: "[A]/////[A]/////[A]".into(),
            rules: r(&[(
                'A',
                &[
                    (1.0, "F[&+AL]F[&-AL]AL"),
                    (1.0, "F[^AL][&AL]L"),
                    (1.0, "FAL"),
                ],
            )]),
            iterations: 5,
            angle: 30.0,
            length: 0.12,
            length_scale: 0.85,
            radius: 0.02,
            radius_scale: 0.75,
            tropism: [0.0, 1.0, 0.0],
            tropism_strength: 0.02,
            leaf_size: [0.06, 0.08],
            leaf: leaf("round", 45.0, 20.0, 20.0, 0.01),
            ..base
        },
        // A fern: five arching fronds of paired leaflets.
        "fern" => LSystem {
            axiom: "[&A]/////[&A]/////[&A]/////[&A]/////[&A]".into(),
            rules: r(&[('A', &[(1.0, "F[+L][-L]A")])]),
            iterations: 12,
            angle: 60.0,
            length: 0.06,
            length_scale: 1.0,
            radius: 0.006,
            radius_scale: 1.0,
            taper: 0.93,
            branch_roll: 0.0,
            jitter: 3.0,
            tropism: [0.0, -1.0, 0.0],
            tropism_strength: 0.04,
            leaf_size: [0.035, 0.13],
            leaf: leaf("lance", 15.0, 25.0, 10.0, 0.0),
            ..base
        },
        // A weed tuft: thin stems that fork and bend over.
        "weed" => LSystem {
            axiom: "[&X]/////[&X]/////[&X]/////X".into(),
            rules: r(&[
                (
                    'X',
                    &[
                        (1.0, "F[+XL]F[-XL]+XL"),
                        (1.0, "F[-XL]F[+XL]-XL"),
                        (1.0, "F[&XL]FXL"),
                    ],
                ),
                ('F', &[(1.0, "FF")]),
            ]),
            iterations: 4,
            angle: 22.0,
            length: 0.025,
            length_scale: 1.0,
            radius: 0.004,
            radius_scale: 0.8,
            tropism_strength: 0.05,
            leaf_size: [0.012, 0.05],
            leaf: leaf("lance", 30.0, 30.0, 30.0, 0.0),
            ..base
        },
        // Roots: grow down and spread (turned upside down: rotation x 180).
        "roots" => LSystem {
            axiom: "[A]///[A]///[A]///[A]".into(),
            rules: r(&[(
                'A',
                &[(1.0, "!F[&A]F[^A]A"), (1.0, "!F[+A]FA"), (1.0, "FA")],
            )]),
            iterations: 5,
            angle: 35.0,
            length: 0.15,
            length_scale: 0.85,
            radius: 0.05,
            radius_scale: 0.75,
            tropism: [0.0, 1.0, 0.0],
            tropism_strength: 0.06,
            rotation: Some([1.0, 0.0, 0.0, 0.0]),
            ..base
        },
        "" | "custom" => base,
        other => {
            return Err(format!(
                "preset: unknown `{other}` (tree, pine, bush, fern, weed, roots)"
            ));
        }
    })
}

impl LSystemSpec {
    fn into_op(self, ctx: &str) -> Result<Op, String> {
        let mut d = lsystem_preset(self.preset.as_deref().unwrap_or(""))
            .map_err(|e| format!("{ctx}: {e}"))?;
        if self.preset.is_none() && self.axiom.is_none() {
            return Err(format!(
                "{ctx}: give a `preset` (tree, pine, bush, fern, weed, roots) or an `axiom` (with `rules` to grow it)"
            ));
        }
        if let Some(a) = self.axiom {
            d.axiom = a;
        }
        if let Some(rules) = self.rules {
            d.rules = rules
                .into_iter()
                .map(|(k, v)| {
                    let c = k;
                    let v: RuleSpec = serde_json::from_value(v).map_err(|e| {
                        format!("{ctx}: rules.{c}: a string or a list of choices ({e})")
                    })?;
                    let choices = match v {
                        RuleSpec::One(to) => vec![crate::lsystem::Choice { p: 1.0, to }],
                        RuleSpec::Many(v) => v
                            .into_iter()
                            .map(|c| match c {
                                ChoiceSpec::Plain(to) => crate::lsystem::Choice { p: 1.0, to },
                                ChoiceSpec::Weighted { p, to } => crate::lsystem::Choice { p, to },
                            })
                            .collect(),
                    };
                    Ok::<_, String>((c, choices))
                })
                .collect::<Result<_, _>>()?;
        }
        macro_rules! over {
            ($($f:ident),*) => { $(if let Some(v) = self.$f { d.$f = v; })* };
        }
        if let Some(c) = self.constants {
            d.constants = c;
        }
        macro_rules! leaf {
            ($($f:ident => $g:ident),*) => { $(if let Some(v) = self.$f { d.leaf.$g = v; })* };
        }
        leaf!(leaf_shape => shape, leaf_angle => angle, leaf_curl => curl, leaf_fold => fold, leaf_stalk => stalk, leaf_mode => mode, leaf_cross => cross);
        over!(
            iterations,
            angle,
            length,
            length_scale,
            radius,
            radius_scale,
            taper,
            branch_roll,
            jitter,
            length_jitter,
            tropism,
            tropism_strength,
            seed,
            sides,
            leaf_size,
            translation
        );
        if self.rotation.is_some() {
            d.rotation = rot(ctx, &self.rotation)?;
        }
        d.validate().map_err(|e| format!("{ctx}: {e}"))?;
        Ok(Op::LSystem {
            name: self.name,
            def: Box::new(d),
        })
    }
}

/// 3D text: letters standing up facing +z (readable from the front), the
/// first line's baseline on y = 0, `depth` thick toward -z. One closed
/// solid, ready for booleans (engrave by subtracting it).
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TextSpec {
    name: String,
    /// The text; `\n` starts a new line.
    text: String,
    /// "sans" (default), "bold", "mono", or a path to a .ttf / .otf file.
    #[serde(default)]
    font: String,
    /// Capital letter height, meters.
    size: f32,
    /// Thickness, meters (default: size / 8).
    #[serde(default)]
    depth: Option<f32>,
    /// "center" (default), "left" or "right" about x = 0.
    #[serde(default)]
    align: Option<String>,
    /// Line height multiplier (1).
    #[serde(default)]
    line_spacing: Option<f32>,
    /// Extra space between letters, as a share of the capital height.
    #[serde(default)]
    letter_spacing: f32,
    #[serde(default, alias = "position")]
    translation: [f32; 3],
    /// Rotation: [x, y, z, w], {"axis": [1, 0, 0], "degrees": -20} or {"x": -20} (degrees).
    #[serde(default)]
    rotation: Option<RotationSpec>,
}

/// A number (a square) or [width, depth].
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum Size2 {
    One(f32),
    Two([f32; 2]),
}

impl Size2 {
    fn get(&self) -> [f32; 2] {
        match self {
            Size2::One(v) => [*v, *v],
            Size2::Two(v) => *v,
        }
    }
}

/// A level pad in a terrain (a building plot, a plaza, a camp).
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PadSpec {
    /// Centre [x, z] in the terrain's own coordinates.
    at: [f32; 2],
    /// Circle radius, meters (or give `size`).
    #[serde(default)]
    radius: Option<f32>,
    /// A rectangle [width, depth] (or one number: a square) instead of a circle.
    #[serde(default)]
    size: Option<Size2>,
    /// Pad top y (default: the ground's own height at `at`).
    #[serde(default)]
    height: Option<f32>,
    /// Meters over which it blends back into the ground (3).
    #[serde(default)]
    falloff: Option<f32>,
}

/// A carved bed along a polyline: a road (depth 0.1-0.3), a river (1-2).
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TerrainPathSpec {
    /// [[x, z], ...] in the terrain's own coordinates.
    points: Vec<[f32; 2]>,
    /// Flat bed width, meters (3).
    #[serde(default)]
    width: Option<f32>,
    /// How far below the ground on either side, meters (0.3).
    #[serde(default)]
    depth: Option<f32>,
    /// Bank width blending back into the ground, meters (= width).
    #[serde(default)]
    falloff: Option<f32>,
}

/// Ground: a closed slab whose top is a noise heightfield at real size,
/// centred on the origin (top between y = 0 and `height`, flat bottom at
/// `base`), with level `flatten` pads and carved `paths`. UVs are metric.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TerrainSpec {
    name: String,
    /// [width (x), depth (z)] meters, or one number for a square (60).
    #[serde(default)]
    size: Option<Size2>,
    /// Meters per grid cell (0.5; at most 1M cells).
    #[serde(default)]
    resolution: Option<f32>,
    /// Relief, meters (6): the highest ground reaches about this y.
    #[serde(default)]
    height: Option<f32>,
    /// Feature size, meters (20): hill / ridge spacing, dune wavelength.
    #[serde(default)]
    scale: Option<f32>,
    /// Noise octaves, 1..10 (5): more = finer detail.
    #[serde(default)]
    octaves: Option<u32>,
    /// Amplitude kept per octave, 0..1 (0.5): higher = rougher.
    #[serde(default)]
    gain: Option<f32>,
    /// "hills" (default), "ridges" (sharp crests), "dunes" (wind-blown
    /// crests, see `wind`), "mesa" (terraced plateaus, see `terraces`),
    /// "flat" (a level slab, for pads and paths only).
    #[serde(default)]
    style: Option<String>,
    /// Same seed = same ground.
    #[serde(default)]
    seed: Option<u64>,
    /// y of the flat bottom (-1); must be below every top point.
    #[serde(default)]
    base: Option<f32>,
    /// mesa: number of terraces (5).
    #[serde(default)]
    terraces: Option<u32>,
    /// dunes: wind direction, degrees about +y (0: crests run along x).
    #[serde(default)]
    wind: Option<f32>,
    /// Meters over which the edges ease down to y = 0 (0 = off): a map border.
    #[serde(default)]
    edge_falloff: Option<f32>,
    /// Level pads: [{"at": [x, z], "radius": 6, "height"?: 0, "falloff"?: 3}].
    #[serde(default)]
    flatten: Vec<PadSpec>,
    /// Carved beds: [{"points": [[x, z], ...], "width": 3, "depth": 0.3}].
    #[serde(default)]
    paths: Vec<TerrainPathSpec>,
    #[serde(default, alias = "position")]
    translation: Option<[f32; 3]>,
    /// Rotation: [x, y, z, w], {"axis": [0, 1, 0], "degrees": 30} or {"y": 30}.
    #[serde(default)]
    rotation: Option<RotationSpec>,
}

impl TerrainSpec {
    fn into_op(self, ctx: &str) -> Result<Op, String> {
        let mut d = crate::terrain::Terrain::default();
        if let Some(s) = &self.size {
            d.size = s.get();
        }
        macro_rules! over {
            ($($f:ident),*) => { $(if let Some(v) = self.$f { d.$f = v; })* };
        }
        over!(
            resolution,
            height,
            scale,
            octaves,
            gain,
            style,
            seed,
            base,
            terraces,
            wind,
            edge_falloff,
            translation
        );
        d.flatten = self
            .flatten
            .into_iter()
            .map(|p| crate::terrain::Pad {
                at: p.at,
                radius: p.radius.unwrap_or(0.0),
                size: p.size.map(|s| s.get()),
                height: p.height,
                falloff: p.falloff.unwrap_or(3.0),
            })
            .collect();
        d.paths = self
            .paths
            .into_iter()
            .map(|p| {
                let width = p.width.unwrap_or(3.0);
                crate::terrain::Path {
                    points: p.points,
                    width,
                    depth: p.depth.unwrap_or(0.3),
                    falloff: p.falloff.unwrap_or(width),
                }
            })
            .collect();
        d.rotation = rot(ctx, &self.rotation)?;
        d.validate().map_err(|e| format!("{ctx}: {e}"))?;
        Ok(Op::Terrain {
            name: self.name,
            def: Box::new(d),
        })
    }
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SweepSpec {
    name: String,
    /// Cross-section [[u, v], ...] (points or arcs, one ring) centred on the
    /// path: u along the frame normal (starts as world up, or +z for a
    /// vertical path), v across. A circle: [{"arc": {"center": [0, 0],
    /// "radius": 0.02, "from": 0, "to": 360}}].
    profile: Vec<RingItem>,
    /// World points [[x, y, z], ...] the profile travels along (bends are
    /// mitred to keep thickness). With smooth, a few control points of a
    /// curve instead.
    path: Vec<[f32; 3]>,
    /// Join the end back to the start (a ring); otherwise both ends are capped.
    #[serde(default)]
    closed: bool,
    /// Round the profile's corners with this radius.
    #[serde(default)]
    round: f32,
    /// true: the path is control points of a smooth curve (centripetal
    /// Catmull-Rom) passing through every one, sampled finely: 4 points
    /// make a hose.
    #[serde(default)]
    smooth: bool,
    /// Same as smooth: "catmull_rom" (or "linear", the default).
    #[serde(default)]
    curve: Option<String>,
    /// Curve segments per span between control points (default: by the
    /// bend, about one per 5 degrees).
    #[serde(default)]
    samples: u32,
    /// Degrees the profile turns about the path from start to end.
    #[serde(default)]
    twist: f32,
    /// Profile scale at the end of the path (1 at the start, by length): a
    /// tapering horn, tentacle or tail.
    #[serde(default = "one")]
    scale_end: f32,
}

fn one() -> f32 {
    1.0
}

/// `smooth` / `curve` of a path item as one flag.
fn curve_smooth(ctx: &str, smooth: bool, curve: &Option<String>) -> Result<bool, String> {
    match curve.as_deref() {
        None | Some("linear") => Ok(smooth),
        Some("catmull_rom" | "smooth") => Ok(true),
        Some(o) => Err(format!(
            "{ctx}: curve: unknown curve `{o}` (catmull_rom or linear)"
        )),
    }
}

/// One cross-section of a loft.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LoftSectionSpec {
    /// One ring [[a, b], ...] (points or arcs). Straight lofts: a is world x
    /// and b world z before `rotation` (like an extrude footprint); on a
    /// path, a is along the path normal (up) and b across, like sweep.
    profile: Vec<RingItem>,
    /// Centre of the section [x, y, z] (straight lofts).
    #[serde(default)]
    at: Option<[f32; 3]>,
    /// Shorthand for at: [0, y, 0].
    #[serde(default)]
    y: Option<f32>,
    /// Fraction 0..1 along the path (path lofts; default evenly spread).
    #[serde(default)]
    t: Option<f32>,
    /// Tilts the section plane (straight lofts): [x, y, z, w], {axis,
    /// degrees} or {x, y, z} degrees. Unrotated sections lie flat (normal +y).
    #[serde(default)]
    rotation: Option<RotationSpec>,
    /// Scales the profile: a number or [a, b]. 0 on the first or last
    /// section closes the loft to a point (a horn or spire tip).
    #[serde(default)]
    scale: Option<ScaleArg>,
}

/// A number or a per-axis pair.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum ScaleArg {
    One(f32),
    Two([f32; 2]),
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LoftSpec {
    name: String,
    /// Two or more cross-sections in order (shapes and sizes may differ: a
    /// circle into a square). Each is resampled to the same point count
    /// with matched start points, so the skin doesn't twist.
    sections: Vec<LoftSectionSpec>,
    /// Optional path [[x, y, z], ...]: sections sit at `t` along it, square
    /// to it (a horn, a tusk, a curved blade). Without it, sections sit at
    /// their own `at` / `y`.
    #[serde(default)]
    path: Vec<[f32; 3]>,
    /// Smooth (Catmull-Rom) between sections and along the path instead of
    /// straight: a bottle's shoulder, a hull's curve.
    #[serde(default)]
    smooth: bool,
    /// Points around every section (default the most any section has, at
    /// least 32; corners of rings with fewer points stay sharp).
    #[serde(default)]
    samples: u32,
    /// Rings between consecutive sections (default 12 when smooth, else 1).
    #[serde(default)]
    steps: u32,
    /// Round every profile's corners with this radius.
    #[serde(default)]
    round: f32,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PathArraySpec {
    /// Copies are name_0..name_{n-1}, copy 0 at the path start; hide the source.
    name: String,
    /// The part to copy, modelled at the origin with +x along the path and
    /// +y up: copy i is the source moved to the i-th point (its own
    /// translation becomes an offset in the path frame).
    input: String,
    /// World points [[x, y, z], ...].
    path: Vec<[f32; 3]>,
    /// true: the path is control points of a smooth curve (centripetal
    /// Catmull-Rom) passing through every one, sampled finely: 4 points
    /// make a hose.
    #[serde(default)]
    smooth: bool,
    /// Same as smooth: "catmull_rom" (or "linear", the default).
    #[serde(default)]
    curve: Option<String>,
    /// Curve segments per span between control points (default: by the
    /// bend, about one per 5 degrees).
    #[serde(default)]
    samples: u32,
    /// Loop the path (a ring of posts, a necklace): copies spread around it
    /// with none doubled at the seam.
    #[serde(default)]
    closed: bool,
    /// How many copies, evenly spread end to end.
    #[serde(default)]
    count: u32,
    /// Or the distance between copies, meters (rounded so both ends of an
    /// open path get one).
    #[serde(default)]
    spacing: f32,
    /// Turn each copy to the path: +x along the tangent, +y up (default
    /// true). false only moves them.
    #[serde(default = "yes")]
    align: bool,
    /// Extra roll per copy about the tangent, degrees: 90 for chain links.
    #[serde(default)]
    roll_step: f32,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HullSpec {
    name: String,
    /// Parts whose convex hull to take (e.g. two spheres = a capsule between
    /// them; eight small spheres = a rounded box). First input's material.
    inputs: Vec<String>,
}

/// One scatter input: a part (or `*` pattern: every matching visible part
/// moves as one set, e.g. a prefab instance `"tree1.*"`), or a list of parts
/// moved as one set (`["oak", "oak_leaves"]`).
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum ScatterEntry {
    One(String),
    Set(Vec<String>),
}

/// A part, or a list of entries (one is picked at random per copy).
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum ScatterInput {
    One(String),
    Many(Vec<ScatterEntry>),
}

/// A number (fixed) or [min, max].
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum RangeArg {
    One(f32),
    Two([f32; 2]),
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RegionSpec {
    /// [x, z] corner.
    min: [f32; 2],
    /// [x, z] corner.
    max: [f32; 2],
}

/// Copies of parts strewn over another part's surface (rocks on terrain,
/// grass tufts, mushrooms on a log, rivets on a hull, a forest): outputs
/// name_0..name_{n-1}, each standing with its base on the surface. Hide the
/// source parts.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScatterSpec {
    name: String,
    /// What to strew: "rock", or ["rock_a", "rock_b"] (one picked per copy),
    /// or sets moved as one body: [["oak", "oak_leaves"], "tree1.*"]. An
    /// lsystem's leaves come along with its wood ("oak" = ["oak", "oak_leaves"]).
    input: ScatterInput,
    /// The part whose surface they land on.
    on: String,
    /// Number of copies (fewer when spacing, slope or region leave no room).
    #[serde(default)]
    count: Option<u32>,
    /// Copies per square meter of eligible surface, instead of count.
    #[serde(default)]
    density: Option<f32>,
    /// Cap with density (default 2000, at most 10000).
    #[serde(default)]
    max: Option<u32>,
    /// Same seed = same layout (default 1).
    #[serde(default)]
    seed: Option<u64>,
    /// Uniform scale: a number or a random [min, max] range (default 1).
    #[serde(default)]
    scale: Option<RangeArg>,
    /// "random_y" (default: spin about up), "random" (any orientation:
    /// tumbled rocks) or "none".
    #[serde(default)]
    rotate: Option<String>,
    /// 0 (default) = stay upright (trees), 1 = stand along the surface
    /// normal (mushrooms on a log, rivets on a hull); between leans partly.
    #[serde(default)]
    align: Option<f32>,
    /// Minimum distance between copies, meters. Default: 0.8 x the widest
    /// copy's footprint (no overlaps); 0 allows clumps.
    #[serde(default)]
    min_spacing: Option<f32>,
    /// Steepest surface used, degrees from level (e.g. 30: no trees on cliffs).
    #[serde(default)]
    max_slope: Option<f32>,
    /// Extra depth pushed into the surface, meters (default 0; copies
    /// already sink until their base is at or below the surface).
    #[serde(default)]
    sink: Option<f32>,
    /// Only points with x, z inside {"min": [x, z], "max": [x, z]}.
    #[serde(default)]
    region: Option<RegionSpec>,
    /// "up" (default: faces pointing up) or "all" (sides and undersides too,
    /// with align 1: mushrooms round a log).
    #[serde(default)]
    faces: Option<String>,
    /// Relative odds per input entry (default equal).
    #[serde(default)]
    weights: Option<Vec<f32>>,
}

impl ScatterSpec {
    fn into_op(self, ctx: &str) -> Result<Op, String> {
        let input: Vec<Vec<String>> = match self.input {
            ScatterInput::One(s) => vec![vec![s]],
            ScatterInput::Many(v) => v
                .into_iter()
                .map(|e| match e {
                    ScatterEntry::One(s) => vec![s],
                    ScatterEntry::Set(v) => v,
                })
                .collect(),
        };
        if input.is_empty() || input.iter().any(Vec::is_empty) {
            return Err(format!(
                "{ctx}: input: name at least one part in every entry"
            ));
        }
        if input.iter().flatten().any(|n| n == &self.on) {
            return Err(format!("{ctx}: input: `{}` is also `on`", self.on));
        }
        let mut def = crate::scatter::ScatterDef::default();
        match (self.count, self.density) {
            (Some(_), Some(_)) => {
                return Err(format!("{ctx}: give count or density, not both"));
            }
            (None, None) => {
                return Err(format!(
                    "{ctx}: count: say how many (count: 40) or how dense (density: 0.5 per m²)"
                ));
            }
            (Some(c), None) => {
                if c > crate::scatter::MAX_COPIES {
                    return Err(format!(
                        "{ctx}: count: at most {} copies",
                        crate::scatter::MAX_COPIES
                    ));
                }
                def.count = Some(c);
            }
            (None, Some(d)) => {
                if !(d.is_finite() && d > 0.0) {
                    return Err(format!("{ctx}: density: must be > 0 per square meter"));
                }
                def.density = Some(d);
            }
        }
        if let Some(m) = self.max {
            def.max = m.min(crate::scatter::MAX_COPIES);
        }
        if let Some(seed) = self.seed {
            def.seed = seed;
        }
        if let Some(r) = self.scale {
            def.scale = match r {
                RangeArg::One(x) => [x, x],
                RangeArg::Two(v) => v,
            };
            if def.scale.iter().any(|x| !(x.is_finite() && *x > 0.0)) {
                return Err(format!("{ctx}: scale: must be > 0"));
            }
        }
        if let Some(r) = self.rotate {
            if !matches!(r.as_str(), "random_y" | "random" | "none") {
                return Err(format!(
                    "{ctx}: rotate: `{r}` (one of random_y, random, none)"
                ));
            }
            def.rotate = r;
        }
        if let Some(a) = self.align {
            if !(0.0..=1.0).contains(&a) {
                return Err(format!("{ctx}: align: 0 (upright) .. 1 (along the normal)"));
            }
            def.align = a;
        }
        if let Some(m) = self.min_spacing {
            if !(m.is_finite() && m >= 0.0) {
                return Err(format!("{ctx}: min_spacing: must be >= 0 meters"));
            }
            def.min_spacing = Some(m);
        }
        if let Some(m) = self.max_slope {
            if !(0.0..=180.0).contains(&m) {
                return Err(format!("{ctx}: max_slope: degrees 0..180"));
            }
            def.max_slope = Some(m);
        }
        if let Some(x) = self.sink {
            def.sink = x;
        }
        if let Some(r) = self.region {
            def.region = Some([
                [r.min[0].min(r.max[0]), r.min[1].min(r.max[1])],
                [r.min[0].max(r.max[0]), r.min[1].max(r.max[1])],
            ]);
        }
        if let Some(f) = self.faces {
            if !matches!(f.as_str(), "up" | "all") {
                return Err(format!("{ctx}: faces: `{f}` (up or all)"));
            }
            def.faces = f;
        }
        if let Some(w) = self.weights {
            if w.len() != input.len() {
                return Err(format!(
                    "{ctx}: weights: {} given for {} input entries",
                    w.len(),
                    input.len()
                ));
            }
            if w.iter().any(|x| !(x.is_finite() && *x >= 0.0)) || w.iter().sum::<f32>() <= 0.0 {
                return Err(format!("{ctx}: weights: >= 0 and not all 0"));
            }
            def.weights = w;
        }
        Ok(Op::Scatter {
            name: self.name,
            input,
            on: self.on,
            def: Box::new(def),
        })
    }
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BlobSpec {
    name: String,
    /// Closed solids to melt together (spheres, capsules, boxes, any mesh).
    /// First input's material; hide the inputs.
    inputs: Vec<String>,
    /// "union" (default: everything melted into one), "subtract" (the rest
    /// carved out of the first with a rounded rim) or "intersect".
    #[serde(default)]
    op: Option<String>,
    /// Fillet radius of the blend in meters (0 = a sharp seam). Default: a
    /// fifth of the smallest input's thinnest size.
    #[serde(default)]
    blend: Option<f32>,
    /// Voxel size in meters. Default: 1/120 of the result's size. Finer is
    /// smoother but slower; refused past ~27M samples.
    #[serde(default)]
    resolution: Option<f32>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlaneSpec {
    /// The side it points to is "above". Default [0, 1, 0] (up).
    #[serde(default)]
    normal: Option<[f32; 3]>,
    /// Any world point on the plane. Default the origin.
    #[serde(default)]
    point: Option<[f32; 3]>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SplitSpec {
    /// Outputs `name_above` and `name_below`.
    name: String,
    /// A closed solid to cut; hide it.
    input: String,
    /// The cutting plane: {"normal": [0, 1, 0], "point": [0, 0.3, 0]}.
    plane: PlaneSpec,
    /// "both" (default), "above" or "below".
    #[serde(default)]
    keep: Option<String>,
    /// Meters between the halves (each moves half of it along the normal).
    #[serde(default)]
    gap: f32,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RoundSpec {
    name: String,
    /// A closed solid to round.
    input: String,
    /// "fillet" (round convex edges by radius, outer size kept), "chamfer"
    /// (45° bevel of radius on axis-aligned edges) or "smooth" (subdivision
    /// rounding of sharp edges; radius ignored).
    mode: String,
    /// Fillet/chamfer size in meters.
    #[serde(default)]
    radius: f32,
    /// Fillet sphere segments (default 16) or smooth refinement steps (default 3).
    #[serde(default)]
    segments: u32,
}

/// A number (both cross axes) or [a, b] (the two cross axes in x, y, z order).
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum ScaleEndArg {
    Both(f32),
    Each([f32; 2]),
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeformSpec {
    name: String,
    /// The part to deform (a closed solid stays closed; sheets work too).
    /// Hide it once the deformed copy is made.
    input: String,
    /// "displace" (noise along the normals: rocks, cliffs, bark, dents),
    /// "bend" (curl the part along `axis` by `degrees` toward `toward`:
    /// horns, tusks, curved blades), "twist" (`degrees` about `axis`
    /// over the part's length) or "taper" (cross-section from 1 to
    /// `scale_end` along `axis`). Chain deforms for a horn: taper, then bend.
    mode: String,
    /// bend/twist/taper: the axis the effect runs along, "x", "y" or "z"
    /// (default "y"); it spans the part's whole extent on that axis.
    #[serde(default)]
    axis: Option<String>,
    /// bend/twist: total angle in degrees from the low end to the high end.
    #[serde(default)]
    degrees: Option<f32>,
    /// bend: where the high end curls toward: "x", "-x", "z", "-z", "y",
    /// "-y" (or right/left/front/back/up/down); not along `axis`. Default
    /// +x (+y when axis is x).
    #[serde(default)]
    toward: Option<String>,
    /// taper: the scale at the high end of `axis` (0.1 = a point-ish tip,
    /// 1.5 = flares out); a number, or [a, b] for the two cross axes.
    #[serde(default)]
    scale_end: Option<ScaleEndArg>,
    /// taper: profile exponent (default 1 = linear; 2 keeps it thick longer
    /// then thins, 0.5 thins early).
    #[serde(default)]
    power: Option<f32>,
    /// displace: largest offset along the normal in meters (default 12% of
    /// the part's smallest size); negative pushes the bumps in.
    #[serde(default)]
    amount: Option<f32>,
    /// displace: noise feature size in meters (default 45% of the part's
    /// largest size: a few big lumps); smaller = more, finer bumps.
    #[serde(default)]
    scale: Option<f32>,
    /// displace: noise octaves, 1..8 (default 4): finer detail on the lumps.
    #[serde(default)]
    octaves: Option<u32>,
    /// displace: noise seed (default 0); same seed = same rock.
    #[serde(default)]
    seed: Option<u32>,
    /// displace: "fbm" (default, rounded lumps), "ridged" (sharp creases:
    /// cliffs, crags, bark) or "turbulence" (billowy dents).
    #[serde(default)]
    noise: Option<String>,
    /// Target edge length in meters: the input is refined to it first so
    /// the deform has vertices to move (a 12-triangle box can't bend).
    /// Default: scale / 8 for displace, 1/32 of the axis length otherwise
    /// (material: two texels of the bake); capped at about 250k triangles.
    #[serde(default)]
    detail: Option<f32>,
    /// material: the procedural material (material_ops / material_catalog
    /// name) whose `height` output becomes real geometry: bark, scales,
    /// wrinkles, cobbles. Give the result the same pbr material so colour
    /// and relief line up. `amount` multiplies the height (default 1: the
    /// height channel's own meters); `scale` is the pattern scale as on a
    /// material item.
    #[serde(default)]
    pbr: Option<String>,
    /// material: param overrides for this use of the pbr material.
    #[serde(default)]
    params: Option<std::collections::BTreeMap<String, f64>>,
}

/// A number or a short list of numbers.
#[derive(serde::Deserialize, schemars::JsonSchema, Clone)]
#[serde(untagged)]
pub enum NumOrList {
    One(f32),
    Many(Vec<f32>),
}

impl NumOrList {
    fn list(self) -> Vec<f32> {
        match self {
            NumOrList::One(x) => vec![x],
            NumOrList::Many(v) => v,
        }
    }
}

/// One shape of an `sdf` item.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SdfShapeSpec {
    /// "sphere" {radius, at}, "ellipsoid" {size: radii [x, y, z], at,
    /// rotation}, "box" {size: full [x, y, z], round, at, rotation},
    /// "capsule" {from, to, radius: r or [r_from, r_to] (a rounded cone)},
    /// "cylinder" {radius, height, round, at, rotation; along y},
    /// "torus" {size: [ring, tube], at, rotation; flat in xz}, "chain"
    /// {points: [[x, y, z, radius], ...], smooth (default true)}: a tube
    /// through the points whose radius varies, "part" {part}: a closed
    /// solid of the model sampled as a field, "group" {shapes}: combined on
    /// their own first, then joined as one (subtract a blended group).
    shape: String,
    /// How it joins the shapes before it: "union" (default), "subtract",
    /// "intersect".
    #[serde(default)]
    op: Option<String>,
    /// Smooth radius of this join in meters (default: the item's blend; 0 =
    /// a sharp seam).
    #[serde(default)]
    blend: Option<f32>,
    /// Centre (default the origin).
    #[serde(default)]
    at: Option<[f32; 3]>,
    #[serde(default)]
    rotation: Option<RotationSpec>,
    #[serde(default)]
    radius: Option<NumOrList>,
    #[serde(default)]
    size: Option<NumOrList>,
    #[serde(default)]
    height: Option<f32>,
    /// box / cylinder: edge rounding radius in meters.
    #[serde(default)]
    round: Option<f32>,
    #[serde(default)]
    from: Option<[f32; 3]>,
    #[serde(default)]
    to: Option<[f32; 3]>,
    #[serde(default)]
    points: Option<Vec<[f32; 4]>>,
    #[serde(default)]
    smooth: Option<bool>,
    #[serde(default)]
    part: Option<String>,
    #[serde(default)]
    shapes: Option<Vec<SdfShapeSpec>>,
}

impl SdfShapeSpec {
    fn into_shape(self, at: &str) -> Result<crate::sdf::Shape, String> {
        let rotation = match self.rotation {
            Some(r) => Some(r.quat().map_err(|e| format!("{at}: rotation: {e}"))?),
            None => None,
        };
        let shapes = match self.shapes {
            Some(v) => Some(
                v.into_iter()
                    .enumerate()
                    .map(|(i, c)| c.into_shape(&format!("{at}.shapes[{i}]")))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            None => None,
        };
        Ok(crate::sdf::Shape {
            shape: self.shape,
            op: self.op,
            blend: self.blend,
            at: self.at,
            rotation,
            radius: self.radius.map(NumOrList::list),
            size: self.size.map(NumOrList::list),
            height: self.height,
            round: self.round,
            from: self.from,
            to: self.to,
            points: self.points,
            smooth: self.smooth,
            part: self.part,
            shapes,
        })
    }
}

/// A field warp of an `sdf` / `skin` item (the `deform` fields).
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WarpSpec {
    /// "twist", "bend", "taper" (the space, over the field's box at that
    /// point of the list) or "displace" (noise added to the surface).
    mode: String,
    #[serde(default)]
    axis: Option<String>,
    #[serde(default)]
    degrees: Option<f32>,
    #[serde(default)]
    toward: Option<String>,
    #[serde(default)]
    scale_end: Option<ScaleEndArg>,
    #[serde(default)]
    power: Option<f32>,
    #[serde(default)]
    amount: Option<f32>,
    #[serde(default)]
    scale: Option<f32>,
    #[serde(default)]
    octaves: Option<u32>,
    #[serde(default)]
    seed: Option<u32>,
    #[serde(default)]
    noise: Option<String>,
}

impl WarpSpec {
    fn into_deform(self) -> crate::deform::Deform {
        crate::deform::Deform {
            mode: self.mode,
            axis: self.axis,
            degrees: self.degrees,
            toward: self.toward,
            scale_end: self.scale_end.map(|v| match v {
                ScaleEndArg::Both(x) => [x, x],
                ScaleEndArg::Each(a) => a,
            }),
            power: self.power,
            amount: self.amount,
            scale: self.scale,
            octaves: self.octaves,
            seed: self.seed,
            noise: self.noise,
            detail: None,
            pbr: None,
        }
    }
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SdfSpec {
    name: String,
    /// Shapes combined in order, each by its `op` (union / subtract /
    /// intersect) with a smooth radius `blend`.
    shapes: Vec<SdfShapeSpec>,
    /// Default smooth radius of every join in meters (default: a fifth of
    /// the thinnest shape).
    #[serde(default)]
    blend: Option<f32>,
    /// "x" (or "z", "xz"): the field is mirrored, so model one side (x >=
    /// 0) of a symmetric creature. Shapes crossing the plane should be
    /// symmetric themselves.
    #[serde(default)]
    mirror: Option<String>,
    /// Warps of the whole field, in order: {"mode": "twist" | "bend" |
    /// "taper" | "displace", ...} with the deform fields (axis, degrees,
    /// toward, scale_end, power, amount, scale, octaves, seed, noise).
    #[serde(default)]
    warps: Option<Vec<WarpSpec>>,
    /// Meshing voxel size in meters (default 1/120 of the size, finer for
    /// thin shapes, about 12M samples at most; explicit up to 27M).
    #[serde(default)]
    resolution: Option<f32>,
}

/// A skin chain point: [x, y, z, radius] or a joint name.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum ChainPtSpec {
    Joint(String),
    Point([f32; 4]),
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SkinSpec {
    name: String,
    /// Chains of points, each [x, y, z, radius] or a joint name: a tube
    /// through them whose radius follows the points. Chains sharing a joint
    /// melt together there (a limb into a body, fingers into a palm).
    chains: Vec<Vec<ChainPtSpec>>,
    /// Named points [x, y, z, radius] chains can share.
    #[serde(default)]
    joints: Option<std::collections::BTreeMap<String, [f32; 4]>>,
    /// Smooth curves through the points (default true); false = straight
    /// rounded cones between them.
    #[serde(default)]
    smooth: Option<bool>,
    /// Smooth radius where chains meet, meters (default half the median
    /// radius).
    #[serde(default)]
    blend: Option<f32>,
    /// "x" / "z" / "xz": mirror the skin (model one side).
    #[serde(default)]
    mirror: Option<String>,
    /// Field warps, as on sdf.
    #[serde(default)]
    warps: Option<Vec<WarpSpec>>,
    /// Meshing voxel size in meters (default from the size and the thinnest
    /// radius).
    #[serde(default)]
    resolution: Option<f32>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SmoothSpec {
    name: String,
    /// The part to smooth (hide it). relax and subdiv take sheets too.
    input: String,
    /// "relax" (Taubin smoothing: evens out lumps and pinches without
    /// shrinking), "remesh" (a closed solid rebuilt with even triangles of
    /// `edge` meters; edges sharper than that round off) or "subdiv"
    /// (Catmull-Clark of a coarse cage: a box becomes a smooth pebble,
    /// creased where faces meet at more than `crease` degrees).
    mode: String,
    /// relax: rounds (default 10); remesh: tangential relax rounds (5).
    #[serde(default)]
    iterations: Option<u32>,
    /// relax: step size, 0..1 (default 0.5).
    #[serde(default)]
    strength: Option<f32>,
    /// remesh: target edge length in meters (default the input's mean).
    #[serde(default)]
    edge: Option<f32>,
    /// subdiv: levels, 1..5 (default 2); each quadruples the faces.
    #[serde(default)]
    levels: Option<u32>,
    /// subdiv: edges whose faces meet at more than this many degrees stay
    /// sharp (default: none, everything rounds).
    #[serde(default)]
    crease: Option<f32>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DecimateSpec {
    name: String,
    /// The dense part to lighten (hide it; it stays in the graph, so the
    /// decimate is re-derived when it changes).
    input: String,
    /// Share of the input's triangles to keep, (0, 1]: 0.05 keeps 5%.
    #[serde(default)]
    ratio: Option<f32>,
    /// Target triangle count instead of a ratio.
    #[serde(default)]
    tris: Option<u32>,
    /// Bake the input's surface onto the result: its normals become the
    /// result's normal map and its offset the height channel, over
    /// whatever material the result wears (exported with the textures).
    #[serde(default)]
    bake: bool,
    /// Bake atlas size in texels, 64..4096 (default 1024).
    #[serde(default)]
    resolution: Option<u32>,
    /// How far (meters) the bake looks from the low surface for the dense
    /// one (default: 1.5 x the largest gap measured, at least 1% of size).
    #[serde(default)]
    cage: Option<f32>,
    /// Normals crease where faces meet at more than this (default 60).
    #[serde(default)]
    crease: Option<f32>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ShellSpec {
    name: String,
    /// A closed solid to hollow (a box makes a cabinet carcass or drawer).
    input: String,
    /// Wall thickness in meters; every face moves in by exactly this.
    thickness: f32,
    /// Faces left open: one word or a list of top, bottom, left, right,
    /// front, back (world axes; front = +z). Default: closed all round.
    #[serde(default)]
    open: OneOrMany,
}

/// A string or a list of strings.
#[derive(serde::Deserialize, schemars::JsonSchema, Default)]
#[serde(untagged)]
pub enum OneOrMany {
    #[default]
    None,
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    fn into_vec(self) -> Vec<String> {
        match self {
            Self::None => Vec::new(),
            Self::One(s) => vec![s],
            Self::Many(v) => v,
        }
    }
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GroupSpec {
    name: String,
    /// Parts to merge into one mesh (no boolean; they may be disjoint). The
    /// result takes the first input's material; hide the inputs if only the
    /// group should render.
    inputs: Vec<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TransformSpec {
    name: String,
    /// An earlier op's output part.
    input: String,
    #[serde(default, alias = "position")]
    translation: Option<[f32; 3]>,
    /// Rotation: [x, y, z, w], {"axis": [1, 0, 0], "degrees": -20} or {"x": -20} (degrees).
    #[serde(default)]
    rotation: Option<RotationSpec>,
    #[serde(default)]
    scale: Option<[f32; 3]>,
    /// true: values are offsets from the input (translation adds, rotation
    /// composes, scale multiplies). false (default): absolute values.
    #[serde(default)]
    relative: bool,
    /// With relative: rotate/scale about this world point (not the part's
    /// origin), e.g. a tyre contact point to tip a whole model.
    #[serde(default)]
    pivot: Option<[f32; 3]>,
}

/// One relative transform applied to many parts at once (a whole model or a
/// sub-assembly): every visible part matching `parts` (exact names or
/// trailing-`*` prefixes; `["*"]` = everything) moves as one rigid body.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MoveSpec {
    /// Optional id; re-issue the same name to adjust this move.
    #[serde(default)]
    name: Option<String>,
    parts: Vec<String>,
    #[serde(default, alias = "position")]
    translation: Option<[f32; 3]>,
    /// Rotation ([x, y, z, w], {"axis", "degrees"} or {"x", "y", "z"} degrees); or give axis + degrees instead.
    #[serde(default)]
    rotation: Option<RotationSpec>,
    /// Rotation axis for `degrees`, e.g. [1, 0, 0].
    #[serde(default)]
    axis: Option<[f32; 3]>,
    #[serde(default)]
    degrees: Option<f32>,
    /// Point the rotation turns about: [x, y, z], or an anchor such as
    /// "bottom-back" (the moved parts' box) or "lid:bottom-back" (another
    /// part's box). Default: the centre of the moved parts' bottom face.
    #[serde(default)]
    pivot: Option<crate::commands::PivotSpec>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlaceSpec {
    /// Optional id; re-issue the same name to adjust.
    #[serde(default)]
    name: Option<String>,
    /// Parts to drop (names / `*` patterns), moved as one body.
    parts: Vec<String>,
    /// The part they land on (its top surface, or the face met when
    /// travelling `toward`).
    on: String,
    /// Where to slide first, across the travel direction; default: where
    /// they are. Down/up: [x, z] (footprint centre). Back/front: [x, y].
    /// Left/right: [z, y].
    #[serde(default)]
    at: Option<[f32; 2]>,
    /// Clearance left from the surface. Default 0 (touching).
    #[serde(default)]
    gap: Option<f32>,
    /// Direction of travel: "down" (default: land on the top surface),
    /// "up" (under a ceiling or shelf), or "back" / "front" / "left" /
    /// "right" to mount against a wall (a frame on the left wall travels
    /// "left").
    #[serde(default)]
    toward: Option<String>,
    /// Each matched part travels on its own and keeps its footprint (43
    /// keys each dropping onto a sloped deck), instead of the set moving as
    /// one body. Not with `at`.
    #[serde(default)]
    each: bool,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BooleanSpec {
    name: String,
    /// union, difference (alias subtract) or intersection.
    op: BoolOp,
    a: String,
    b: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RadialArraySpec {
    /// Output prefix: copies are name_1..name_{count-1} (name_0.. with
    /// include_source false).
    name: String,
    input: String,
    /// Total items including the source (with include_source).
    count: u32,
    #[serde(default)]
    center: [f32; 3],
    /// Arc to spread items over. Default 360.
    #[serde(default = "default_tool_degrees")]
    degrees: f32,
    #[serde(default)]
    start_degrees: f32,
    /// "y" (default), "x" or "z" (horizontal rings: windmill blades).
    #[serde(default = "default_axis")]
    axis: Axis,
    /// Default true: the source is item 0 and `count` items spread evenly.
    /// false: `count` new copies starting at start_degrees, source untouched.
    #[serde(default = "yes")]
    include_source: bool,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LinearArraySpec {
    /// Output prefix: copies are name_1..name_{count-1}; the source is copy 0.
    name: String,
    input: String,
    count: u32,
    /// Per-copy step; copy i sits at input + i * offset.
    #[serde(default)]
    offset: [f32; 3],
    /// Step `offset` along the source part's own (rotated) axes: lines
    /// stepping down a tilted page.
    #[serde(default)]
    local: bool,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MirrorSpec {
    name: String,
    input: String,
    /// "x", "y" or "z".
    axis: Axis,
    /// The plane sits at axis = offset[axis].
    #[serde(default)]
    offset: [f32; 3],
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MaterialSpec {
    /// The part to colour (in place), or a new output when `input` is given.
    name: String,
    /// Source part; defaults to `name` (colour in place). Copies made from
    /// the part inherit the colour wherever this op sits.
    #[serde(default)]
    input: Option<String>,
    #[serde(default, alias = "color")]
    base_color: Option<ColorArg>,
    #[serde(default)]
    roughness: Option<f32>,
    #[serde(default)]
    metalness: Option<f32>,
    /// A procedural material (material_ops / material_catalog name): its
    /// textures, mapped at real scale (tile) or onto a packed atlas (object).
    #[serde(default)]
    pbr: Option<String>,
    /// Param overrides for this use of the pbr material.
    #[serde(default)]
    params: Option<std::collections::BTreeMap<String, f64>>,
    /// Pattern scale for the pbr material (2 = twice as big).
    #[serde(default)]
    scale: Option<f32>,
    /// A plain colour on purpose (the user wants it flat): no "only a flat
    /// colour" warning for this part.
    #[serde(default)]
    flat: bool,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PartSpec {
    #[serde(alias = "name")]
    part: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RenameSpec {
    part: String,
    to: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeleteSpec {
    /// The op output (or baked part) to remove.
    name: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TriMeshSpec {
    name: String,
    verts: Vec<[f32; 3]>,
    /// Three indices per triangle.
    indices: Vec<u32>,
    #[serde(default)]
    normals: Option<Vec<[f32; 3]>>,
    #[serde(default)]
    uvs: Option<Vec<[f32; 2]>>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UvsSpec {
    part: String,
    /// box, planar, cylindrical, spherical or metric (box at real scale: uv in meters).
    projection: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PrefabSpec {
    /// Output prefix: the prefab's visible parts become name.part.
    name: String,
    /// Catalog entry to instance (see catalog_list).
    prefab: String,
    #[serde(default, alias = "position")]
    translation: [f32; 3],
    /// Rotation: [x, y, z, w], {"axis": [1, 0, 0], "degrees": -20} or {"x": -20} (degrees).
    #[serde(default)]
    rotation: Option<RotationSpec>,
    #[serde(default)]
    scale: Option<[f32; 3]>,
    /// "scene": re-derive the prefab with the scene's style layered over its
    /// own (scene tokens win); an object: a token patch for this instance
    /// ({"colors": {"primary": "#c33"}}). Omitted: the prefab's own style.
    #[serde(default)]
    #[schemars(with = "Option<serde_json::Value>")]
    style: Option<serde_json::Value>,
    /// Game components from the project's vocabulary (.modeler/components.json):
    /// {"Place": {"name": "wendys"}}. Written by a .bsn scene export.
    #[serde(default)]
    #[schemars(with = "Option<serde_json::Value>")]
    components: serde_json::Map<String, Value>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DecalSpec {
    name: String,
    /// The decal's look: a procedural material of domain "decal" (or
    /// "card"): one image laid over the decal, `opacity` cutting its shape,
    /// optionally normal, roughness / metallic and emissive.
    pbr: String,
    /// Param overrides for this use of the material.
    #[serde(default)]
    params: Option<std::collections::BTreeMap<String, f64>>,
    /// [width, height] of the image on the surface, or [width, height,
    /// depth] (depth: how far the box reaches along the projection, default
    /// half the smaller side; deep boxes reach round curves but also onto
    /// anything near).
    size: Vec<f32>,
    /// Stick it on this part: cast `toward` (default down) through `at`
    /// and centre the box where it meets the surface.
    #[serde(default)]
    on: Option<String>,
    /// With `on`: the cross axes, as for `place` (down / up: [x, z];
    /// back / front: [x, y]; left / right: [z, y]). Default: the part's centre.
    #[serde(default)]
    at: Option<[f32; 2]>,
    /// With `on`: down (default), up, back, front, left or right.
    #[serde(default)]
    toward: Option<String>,
    /// Without `on`: the box centre...
    #[serde(default, alias = "position")]
    translation: [f32; 3],
    /// ...and the projection direction, into the surface (default down).
    #[serde(default)]
    direction: Option<[f32; 3]>,
    /// Turns the image about the projection axis (degrees).
    #[serde(default)]
    roll: f32,
    /// A part the decal rides with through moves and places (without `on`).
    #[serde(default)]
    attach: Option<String>,
    /// "clustered" (default: projects onto any surface; needs bindless
    /// textures, not WebGL2 / WebGPU) or "forward" (a quad fading into the
    /// surface, for those targets: suits flat ground and walls).
    #[serde(default)]
    kind: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LightSpec {
    name: String,
    /// point, spot or directional.
    kind: String,
    /// Colour (default white); style tokens work ("@glow").
    #[serde(default)]
    #[schemars(with = "Option<serde_json::Value>")]
    color: Option<Value>,
    /// Lumens for point/spot (a 60 W bulb ~800), lux for directional
    /// (overcast ~1000, full sun ~100000). Defaults: point/spot 2000, directional 3000.
    #[serde(default)]
    intensity: Option<f32>,
    /// Point/spot reach in meters.
    #[serde(default)]
    range: Option<f32>,
    #[serde(default, alias = "position")]
    translation: [f32; 3],
    /// Spot/directional aim (default [0, -1, 0], straight down).
    #[serde(default)]
    direction: Option<[f32; 3]>,
    /// Spot cone half-angles (default inner 0.8 * outer, outer 35).
    #[serde(default)]
    inner_degrees: Option<f32>,
    #[serde(default)]
    outer_degrees: Option<f32>,
    /// Casts shadows (costly in games: use on a few key lights).
    #[serde(default)]
    shadows: bool,
    /// A part the light rides with through moves and places.
    #[serde(default)]
    attach: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ColliderSpec {
    /// The rule's name (re-issue to change it; later rules win per part).
    name: String,
    /// Part names or `*` globs ("wall*", "crate1.*", "*").
    parts: Vec<String>,
    /// box (the part's own box, turned with the part; cheapest), hull (convex hull of the part), mesh
    /// (the render triangles: static level geometry only) or none (no
    /// collider, e.g. to exempt decals from a "*" rule).
    shape: String,
    /// static (default), dynamic or kinematic.
    #[serde(default)]
    body: Option<String>,
    /// A trigger: reports overlaps, blocks nothing.
    #[serde(default)]
    sensor: bool,
    /// Also give hidden parts a collider: they export as a collider node
    /// with no render mesh (invisible blockers, trigger volumes).
    #[serde(default)]
    hidden: bool,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MarkerSpec {
    name: String,
    /// What it is for game code: spawn, waypoint, trigger, camera, item, ...
    kind: String,
    #[serde(default, alias = "position")]
    translation: [f32; 3],
    /// Facing in degrees about +y (0 faces +z / front, 90 faces +x).
    #[serde(default)]
    yaw: Option<f32>,
    /// A box volume [x, y, z] centred on the marker (triggers, zones).
    #[serde(default)]
    size: Option<[f32; 3]>,
    /// Free-form JSON passed through to game code.
    #[serde(default)]
    #[schemars(with = "Option<serde_json::Value>")]
    data: Option<Value>,
    /// A part the marker rides with through moves and places.
    #[serde(default)]
    attach: Option<String>,
    /// A catalog prefab drawn at the marker (its origin on the marker, turned
    /// by yaw) in the window and agent views only, never exported: shows
    /// what game code builds there ("sp-warehouse-exterior").
    #[serde(default)]
    preview: Option<String>,
    /// Game components from the project's vocabulary (.modeler/components.json):
    /// {"Place": {"name": "wendys"}}. Written by a .bsn scene export.
    #[serde(default)]
    #[schemars(with = "Option<serde_json::Value>")]
    components: serde_json::Map<String, Value>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BoneSpec {
    /// The parent bone (none = a root).
    #[serde(default)]
    parent: Option<String>,
    /// The pivot: [x, y, z], or an anchor on a part's box ("leg_l:top").
    head: crate::anim::PointRef,
    /// The far end (default: the children's heads, else the parent
    /// direction continued). Sets the length and the weight envelope.
    #[serde(default)]
    tail: Option<crate::anim::PointRef>,
    /// Envelope radius in meters for smooth weights (default a fifth of
    /// the length): about the limb's thickness.
    #[serde(default)]
    radius: Option<f32>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SkeletonSpec {
    name: String,
    /// Bones by name. Rest frames are world-aligned: a clip's rotation
    /// `{"x": 25}` pitches the bone about world x from rest.
    #[serde(default)]
    bones: std::collections::BTreeMap<String, BoneSpec>,
    /// "x": every `*_l` bone gets a `*_r` twin mirrored across x = 0.
    #[serde(default)]
    mirror: Option<String>,
    /// Auto-rig from an op instead of listing bones: an `lsystem` (bones
    /// along its branches: `trunk_0..`, `branch7_0..`) or a `skin` (a bone
    /// per chain point, named joints keep their names, mirrored skins get
    /// `_l` / `_r` twins). Extra `bones` may hang off the generated ones.
    #[serde(default)]
    from: Option<String>,
    /// lsystem: meters per bone along a branch (default 0.5).
    #[serde(default)]
    bone_length: Option<f32>,
    /// lsystem: deepest branch level that gets bones (0 = trunk only,
    /// default 3); finer twigs ride the bone they grow from.
    #[serde(default)]
    max_depth: Option<u32>,
    /// lsystem: bone budget (default 128, at most 256 for Bevy's GPU
    /// skinning); the thickest branches get bones first.
    #[serde(default)]
    max_bones: Option<u32>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BoxSpec {
    min: [f32; 3],
    max: [f32; 3],
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PaintSpec {
    bone: String,
    /// A world box: every vertex inside starts fully on `bone`.
    region: BoxSpec,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BindSpec {
    name: String,
    skeleton: String,
    /// Parts to bind (`*` globs).
    parts: Vec<String>,
    /// Rigid: the bone the parts ride (a door on its hinge bone).
    #[serde(default)]
    bone: Option<String>,
    /// rigid (default with `bone`) or smooth (default without: per-vertex
    /// weights from the nearest bone envelopes, blended over the mesh), or
    /// auto (for a skeleton `from` an lsystem: exact weights along the
    /// branches, each leaf rigid on its twig's bone; otherwise smooth).
    #[serde(default)]
    mode: Option<String>,
    /// Smooth: meters over which weights blend across a joint (default the
    /// median bone radius).
    #[serde(default)]
    blend: Option<f32>,
    /// Smooth: force regions onto a bone before blending.
    #[serde(default)]
    paint: Vec<PaintSpec>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SwaySpec {
    name: String,
    skeleton: String,
    /// wind (default: trees, grass, cloth-like sway as a function of time)
    /// or spring (secondary motion: tails, ears, antennae, hanging signs
    /// lagging behind `over` and pushed by wind / gravity).
    #[serde(default)]
    kind: Option<String>,
    /// A clip this motion layers over (a spring tail over `walk`; wind over
    /// an idle). Its duration becomes the default `duration`.
    #[serde(default)]
    over: Option<String>,
    /// Bones that sway (`*` globs, default all); the others only follow.
    #[serde(default)]
    bones: Vec<String>,
    /// Seconds of the baked loop (default 4, or the `over` clip's;
    /// with `over` it must be a whole number of its cycles).
    #[serde(default)]
    duration: Option<f32>,
    /// Export samples per second (default 30).
    #[serde(default)]
    fps: Option<f32>,
    /// Wind: where it blows to (horizontal part, default [1, 0, 0]).
    #[serde(default)]
    direction: Option<[f32; 3]>,
    /// Wind: 0.5 a breeze (default for wind), 1 strong, 2 a gale. A spring
    /// gets wind only when this is given.
    #[serde(default)]
    strength: Option<f32>,
    /// Wind: how much gusts vary the strength, 0..1 (default 0.3).
    #[serde(default)]
    gust: Option<f32>,
    /// Wind: fast jitter of the thinnest bones and tips, 0..1 (default 0.5).
    #[serde(default)]
    flutter: Option<f32>,
    /// Wind: speed factor (default 1).
    #[serde(default)]
    frequency: Option<f32>,
    #[serde(default)]
    seed: Option<u32>,
    /// Spring: pull toward the animated pose (default 80; higher = stiffer).
    #[serde(default)]
    stiffness: Option<f32>,
    /// Spring: damping (default 8; 2 * sqrt(stiffness) = no bounce).
    #[serde(default)]
    damping: Option<f32>,
    /// Spring: share of gravity pulling tips down (default 0).
    #[serde(default)]
    gravity: Option<f32>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClipMirrorSpec {
    /// The bone whose track is copied (mirrored across x = 0).
    from: String,
    /// Time shift as a share of the clip (0.5 = half a cycle later).
    #[serde(default)]
    phase: f32,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClipSpec {
    name: String,
    skeleton: String,
    /// Seconds.
    duration: f32,
    /// Loops (default true): t wraps, and smooth keys pass through the seam.
    #[serde(default, rename = "loop")]
    looping: Option<bool>,
    /// Default key interpolation: smooth (Catmull-Rom, default), linear, step.
    #[serde(default)]
    interp: Option<String>,
    /// Per bone: {"rotate" | "translate" | "scale": channel}. A channel is
    /// keys `[[t, value], ...]` (rotate values in any rotation form, offsets
    /// from rest; translate [x, y, z] meters; scale [x, y, z] or a number),
    /// `{"keys": [...], "interp": "linear"}`, or a generated
    /// `{"wave": {"axis": "x" | [x, y, z], "amount" (degrees for rotate,
    /// meters for translate), "period" (s, default the duration), "phase"}}`.
    #[schemars(with = "std::collections::BTreeMap<String, serde_json::Value>")]
    #[serde(default)]
    tracks: std::collections::BTreeMap<String, Value>,
    /// Per bone: a mirrored, phase-shifted copy of another bone's track
    /// (`{"leg_r": {"from": "leg_l", "phase": 0.5}}`).
    #[serde(default)]
    mirror: std::collections::BTreeMap<String, ClipMirrorSpec>,
}

fn axis_of(ctx: &str, v: &Value) -> Result<[f32; 3], String> {
    match v {
        Value::String(s) => match s.as_str() {
            "x" => Ok([1.0, 0.0, 0.0]),
            "y" => Ok([0.0, 1.0, 0.0]),
            "z" => Ok([0.0, 0.0, 1.0]),
            other => Err(format!("{ctx}: axis `{other}`: use x, y, z or [x, y, z]")),
        },
        other => {
            let a: [f32; 3] = de(ctx, other)?;
            if crate::Vec3::from(a).length() < 1e-9 {
                return Err(format!("{ctx}: axis is zero"));
            }
            Ok(a)
        }
    }
}

fn interp_of(ctx: &str, s: &str) -> Result<crate::anim::Interp, String> {
    use crate::anim::Interp;
    match s {
        "smooth" | "catmull_rom" | "cubic" => Ok(Interp::Smooth),
        "linear" => Ok(Interp::Linear),
        "step" => Ok(Interp::Step),
        other => Err(format!(
            "{ctx}: interp `{other}`: use smooth, linear or step"
        )),
    }
}

/// One clip channel: keys or a wave.
fn channel(
    ctx: &str,
    kind: &str,
    v: &Value,
    duration: f32,
    default: crate::anim::Interp,
) -> Result<crate::anim::Channel, String> {
    use crate::anim::Channel;
    let (keys_v, interp) = match v {
        Value::Array(_) => (v.clone(), default),
        Value::Object(o) if o.contains_key("wave") => {
            if o.len() > 1 {
                return Err(format!("{ctx}: a wave channel has only \"wave\""));
            }
            let Some(Value::Object(w)) = o.get("wave") else {
                return Err(format!("{ctx}: wave: expected an object"));
            };
            for k in w.keys() {
                if !matches!(
                    k.as_str(),
                    "axis" | "amount" | "degrees" | "period" | "phase"
                ) {
                    return Err(format!(
                        "{ctx}: wave: unknown field `{k}` (axis, amount / degrees, period, phase)"
                    ));
                }
            }
            let axis = axis_of(ctx, w.get("axis").ok_or(format!("{ctx}: wave needs axis"))?)?;
            let amount = w
                .get("amount")
                .or_else(|| w.get("degrees"))
                .and_then(Value::as_f64)
                .ok_or(format!(
                    "{ctx}: wave needs amount (degrees for rotate, meters for translate)"
                ))? as f32;
            let period = w
                .get("period")
                .and_then(Value::as_f64)
                .map(|p| p as f32)
                .unwrap_or(duration);
            if period.is_nan() || period <= 0.0 {
                return Err(format!("{ctx}: wave period must be positive"));
            }
            let phase = w.get("phase").and_then(Value::as_f64).unwrap_or(0.0) as f32;
            return Ok(Channel::Wave {
                axis,
                amount,
                period,
                phase,
            });
        }
        Value::Object(o) => {
            for k in o.keys() {
                if !matches!(k.as_str(), "keys" | "interp") {
                    return Err(format!(
                        "{ctx}: unknown field `{k}` (keys, interp, or wave)"
                    ));
                }
            }
            let interp = match o.get("interp").and_then(Value::as_str) {
                Some(s) => interp_of(ctx, s)?,
                None => default,
            };
            (
                o.get("keys").cloned().ok_or(format!("{ctx}: needs keys"))?,
                interp,
            )
        }
        other => {
            return Err(format!(
                "{ctx}: expected keys [[t, value], ...], {{keys, interp}} or {{wave}}, got {}",
                describe_json(other)
            ));
        }
    };
    let Value::Array(items) = keys_v else {
        return Err(format!("{ctx}: keys must be an array of [t, value]"));
    };
    let mut keys: Vec<(f32, [f32; 4])> = Vec::new();
    for (k, item) in items.iter().enumerate() {
        let kctx = format!("{ctx}: key #{k}");
        let Some([t, val]) = item
            .as_array()
            .map(Vec::as_slice)
            .and_then(|a| <&[Value; 2]>::try_from(a).ok())
        else {
            return Err(format!("{kctx}: expected [t, value]"));
        };
        let t = t.as_f64().ok_or(format!("{kctx}: time must be a number"))? as f32;
        if t.is_nan() || t < -1e-6 || t > duration + 1e-4 {
            return Err(format!("{kctx}: time {t} is outside 0..{duration}"));
        }
        let v = match kind {
            "rotate" => {
                let r: RotationSpec = de(&kctx, val)?;
                r.quat().map_err(|e| format!("{kctx}: {e}"))?
            }
            "scale" if val.is_number() => {
                let f = val.as_f64().unwrap_or(1.0) as f32;
                [f, f, f, 0.0]
            }
            _ => {
                let a: [f32; 3] = de(&kctx, val)?;
                [a[0], a[1], a[2], 0.0]
            }
        };
        keys.push((t, v));
    }
    if keys.is_empty() {
        return Err(format!("{ctx}: needs at least one key"));
    }
    keys.sort_by(|a, b| a.0.total_cmp(&b.0));
    if keys.windows(2).any(|w| (w[1].0 - w[0].0).abs() < 1e-6) {
        return Err(format!("{ctx}: two keys at the same time"));
    }
    Ok(Channel::Keys { keys, interp })
}

/// One `apply_ops` item, tagged by "type". Only used for the published JSON
/// schema; parsing goes item by item through [`parse_ops`] so errors name the
/// item and the field.
#[allow(dead_code)]
#[derive(schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OpSpec {
    Primitive(PrimitiveSpec),
    Extrude(ExtrudeSpec),
    Revolve(RevolveSpec),
    Group(GroupSpec),
    Sweep(SweepSpec),
    Loft(LoftSpec),
    PathArray(PathArraySpec),
    Lsystem(LSystemSpec),
    Text(TextSpec),
    Terrain(TerrainSpec),
    Hull(HullSpec),
    Blob(BlobSpec),
    Split(SplitSpec),
    Scatter(ScatterSpec),
    Round(RoundSpec),
    Deform(DeformSpec),
    Sdf(SdfSpec),
    Skin(SkinSpec),
    Smooth(SmoothSpec),
    Decimate(DecimateSpec),
    Shell(ShellSpec),
    Transform(TransformSpec),
    Move(MoveSpec),
    Light(LightSpec),
    Decal(DecalSpec),
    Collider(ColliderSpec),
    Marker(MarkerSpec),
    Skeleton(SkeletonSpec),
    Bind(BindSpec),
    Clip(ClipSpec),
    Sway(SwaySpec),
    Place(PlaceSpec),
    Boolean(BooleanSpec),
    RadialArray(RadialArraySpec),
    LinearArray(LinearArraySpec),
    Mirror(MirrorSpec),
    Material(MaterialSpec),
    HidePart(PartSpec),
    ShowPart(PartSpec),
    Rename(RenameSpec),
    Delete(DeleteSpec),
    TriMesh(TriMeshSpec),
    Uvs(UvsSpec),
    Prefab(PrefabSpec),
}

pub const OP_TYPES: &[&str] = &[
    "repeat",
    "light",
    "decal",
    "collider",
    "marker",
    "skeleton",
    "bind",
    "clip",
    "sway",
    "primitive",
    "extrude",
    "revolve",
    "group",
    "sweep",
    "loft",
    "path_array",
    "lsystem",
    "text",
    "terrain",
    "hull",
    "blob",
    "split",
    "scatter",
    "round",
    "deform",
    "sdf",
    "skin",
    "smooth",
    "decimate",
    "shell",
    "transform",
    "move",
    "place",
    "boolean",
    "radial_array",
    "linear_array",
    "mirror",
    "material",
    "hide_part",
    "show_part",
    "rename",
    "delete",
    "tri_mesh",
    "uvs",
    "prefab",
];

/// A rotation in any of three forms: a quaternion `[x, y, z, w]`,
/// `{"axis": [1, 0, 0], "degrees": -20}`, or `{"x": -20, "y": 90, "z": 0}`
/// (degrees about the world axes, applied x, then y, then z).
#[derive(Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum RotationSpec {
    Quat([f32; 4]),
    AxisAngle(AxisAngle),
    Euler(Euler),
}

#[derive(Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AxisAngle {
    axis: [f32; 3],
    degrees: f32,
}

#[derive(Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Euler {
    #[serde(default)]
    x: f32,
    #[serde(default)]
    y: f32,
    #[serde(default)]
    z: f32,
}

impl RotationSpec {
    pub fn quat(&self) -> Result<[f32; 4], String> {
        use crate::{Quat, Vec3};
        Ok(match self {
            RotationSpec::Quat(q) => {
                let n = Quat::from_array(*q);
                if n.length().is_nan() || n.length() <= 1e-6 {
                    return Err("rotation: quaternion is zero".into());
                }
                n.normalize().to_array()
            }
            RotationSpec::AxisAngle(AxisAngle { axis, degrees }) => {
                let a = Vec3::from(*axis);
                if a.length() < 1e-9 {
                    return Err("rotation: axis is zero".into());
                }
                Quat::from_axis_angle(a.normalize(), degrees.to_radians()).to_array()
            }
            RotationSpec::Euler(Euler { x, y, z }) => (Quat::from_rotation_z(z.to_radians())
                * Quat::from_rotation_y(y.to_radians())
                * Quat::from_rotation_x(x.to_radians()))
            .to_array(),
        })
    }
}

/// An optional rotation as a quaternion, with the item named in errors.
fn rot(ctx: &str, r: &Option<RotationSpec>) -> Result<Option<[f32; 4]>, String> {
    r.as_ref()
        .map(|r| r.quat().map_err(|e| format!("{ctx}: {e}")))
        .transpose()
}

/// A loop over items, expanded before anything else reads the batch.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RepeatSpec {
    /// The loop variable: `{r}` in any string becomes its value ("key{r}"),
    /// and `$r` in an expression or as a whole value is the number
    /// ("=$h + $r * 0.0025"). `{r_i}` / `$r_i` is the index (0, 1, ...).
    var: String,
    /// Run for 0..count (the value is the index).
    #[serde(default)]
    count: Option<u32>,
    /// Or run once per value (numbers or strings).
    #[serde(default)]
    values: Option<Vec<Value>>,
    /// The items to repeat; they may hold repeats themselves (nested loops).
    items: Vec<Value>,
}

/// Expands every `repeat` item (nested ones too) into copies of its items
/// with the loop variable substituted. Each output carries a note naming
/// the repeat it came from, for error messages. Two copies making the same
/// part (same type and name) are an error: the name needs `{var}`.
/// `count` and `values` may read params (`"=$rows - 1"`).
pub fn expand_repeats(
    items: &[Value],
    params: &crate::params::Params,
) -> Result<Vec<(Value, Option<String>)>, String> {
    let mut out = Vec::new();
    for (i, item) in items.iter().enumerate() {
        expand_one(item, &format!("op #{i} (repeat)"), None, params, &mut out)?;
    }
    Ok(out)
}

fn expand_one(
    item: &Value,
    ctx: &str,
    note: Option<&str>,
    params: &crate::params::Params,
    out: &mut Vec<(Value, Option<String>)>,
) -> Result<(), String> {
    if item.get("type").and_then(Value::as_str) != Some("repeat") {
        out.push((item.clone(), note.map(str::to_string)));
        return Ok(());
    }
    let mut body = item.clone();
    if let Some(o) = body.as_object_mut() {
        o.remove("type");
        for k in ["count", "values"] {
            if let Some(v) = o.get_mut(k) {
                *v = crate::params::resolve(v, params).map_err(|e| format!("{ctx}: {k}: {e}"))?;
            }
        }
    }
    let r: RepeatSpec = de(ctx, &body)?;
    let var = r.var.trim_start_matches('$');
    if var.is_empty() || !var.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(format!(
            "{ctx}: var: `{}` must be a plain name (letters, digits, _)",
            r.var
        ));
    }
    let values: Vec<Value> = match (r.count, r.values) {
        (Some(n), None) => (0..n).map(Value::from).collect(),
        (None, Some(v)) => v,
        (Some(_), Some(_)) => return Err(format!("{ctx}: give count or values, not both")),
        (None, None) => return Err(format!("{ctx}: needs count or values")),
    };
    if r.items.is_empty() {
        return Err(format!("{ctx}: items is empty"));
    }
    let mut seen: std::collections::HashMap<(String, String), String> =
        std::collections::HashMap::new();
    for (idx, value) in values.iter().enumerate() {
        if !(value.is_number() || value.is_string()) {
            return Err(format!("{ctx}: values[{idx}] must be a number or a string"));
        }
        let shown = match value {
            Value::String(s) => s.clone(),
            v => v.to_string(),
        };
        let here = format!("repeat {var}={shown}");
        let note = match note {
            Some(n) => format!("{n}, {here}"),
            None => here.clone(),
        };
        let binds = [
            (format!("{var}_i"), Value::from(idx)),
            (var.to_string(), value.clone()),
        ];
        for (k, inner) in r.items.iter().enumerate() {
            let v = substitute(inner, &binds);
            let before = out.len();
            expand_one(
                &v,
                &format!("{ctx} > {here} > item {k}"),
                Some(&note),
                params,
                out,
            )?;
            for (made, _) in &out[before..] {
                let (Some(t), Some(n)) = (
                    made.get("type").and_then(Value::as_str),
                    made.get("name").and_then(Value::as_str),
                ) else {
                    continue;
                };
                let key = (t.to_string(), n.to_string());
                if let Some(prev) = seen.get(&key) {
                    return Err(format!(
                        "{ctx}: {prev} and {here} both make {t} `{n}`: put {{{var}}} in its name"
                    ));
                }
                seen.insert(key, here.clone());
            }
        }
    }
    Ok(())
}

/// `{name}` and `$name` substituted in every string of `v`; a string that is
/// exactly `$name` or `{name}` becomes the value itself (a number stays a
/// number).
fn substitute(v: &Value, binds: &[(String, Value)]) -> Value {
    match v {
        Value::String(s) => {
            for (k, val) in binds {
                if *s == format!("${k}") || *s == format!("{{{k}}}") {
                    return val.clone();
                }
            }
            let mut s = s.clone();
            for (k, val) in binds {
                let text = match val {
                    Value::String(t) => t.clone(),
                    Value::Number(n) => match n.as_f64() {
                        Some(x) if x < 0.0 => format!("({n})"),
                        _ => n.to_string(),
                    },
                    other => other.to_string(),
                };
                s = s.replace(&format!("{{{k}}}"), &text);
                s = replace_ref(&s, k, &text);
            }
            Value::String(s)
        }
        Value::Array(a) => Value::Array(a.iter().map(|x| substitute(x, binds)).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| (k.clone(), substitute(x, binds)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Replaces `$name` where the next character does not continue an
/// identifier (`$r` leaves `$row` and `$r_i` alone).
fn replace_ref(s: &str, name: &str, with: &str) -> String {
    let pat = format!("${name}");
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find(&pat) {
        let after = &rest[at + pat.len()..];
        let continues = after
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        out.push_str(&rest[..at]);
        out.push_str(if continues { &pat } else { with });
        rest = after;
    }
    out.push_str(rest);
    out
}

fn de<T: DeserializeOwned>(ctx: &str, body: &Value) -> Result<T, String> {
    serde_path_to_error::deserialize(body).map_err(|e| {
        let path = e.path().to_string();
        if path == "." {
            format!("{ctx}: {}", e.inner())
        } else {
            format!("{ctx}: {path}: {}", e.inner())
        }
    })
}

fn describe_json(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// Parses every `apply_ops` item up front, so a malformed batch fails before
/// anything is applied, with errors like
/// `op #3 (primitive "roof"): size: cylinder size is [radius, height] ...`.
pub fn parse_ops(items: &[Value]) -> Result<Vec<BatchItem>, String> {
    items
        .iter()
        .enumerate()
        .map(|(i, v)| parse_item(i, v))
        .collect()
}

pub fn parse_item(i: usize, v: &Value) -> Result<BatchItem, String> {
    let Some(obj) = v.as_object() else {
        return Err(format!(
            "op #{i}: expected an object with a \"type\" field, got {}",
            describe_json(v)
        ));
    };
    let Some(ty) = obj.get("type").and_then(Value::as_str) else {
        return Err(format!(
            "op #{i}: missing \"type\" (one of: {})",
            OP_TYPES.join(", ")
        ));
    };
    let label = obj
        .get("name")
        .or_else(|| obj.get("part"))
        .and_then(Value::as_str);
    let ctx = match label {
        Some(n) => format!("op #{i} ({ty} \"{n}\")"),
        None => format!("op #{i} ({ty})"),
    };
    let mut body = obj.clone();
    body.remove("type");
    let body = Value::Object(body);
    Ok(match ty {
        "primitive" => {
            let s: PrimitiveSpec = de(&ctx, &body)?;
            let size = resolve_size(s.kind, &s.size.0).map_err(|e| format!("{ctx}: size: {e}"))?;
            BatchItem::Op(Op::Primitive {
                name: s.name,
                kind: s.kind,
                size,
                translation: s.translation,
                rotation: rot(&ctx, &s.rotation)?,
                frame: match s.frame {
                    Some(f) => {
                        if let Some((p, words)) = f.split_once(':') {
                            if p.is_empty() {
                                return Err(format!("{ctx}: frame: `{f}` names no part"));
                            }
                            crate::commands::anchor_point(
                                words,
                                bevy::math::Vec3::ZERO,
                                bevy::math::Vec3::ONE,
                            )
                            .map_err(|e| format!("{ctx}: frame: {e}"))?;
                        }
                        Some(f)
                    }
                    None => None,
                },
            })
        }
        "extrude" => {
            let s: ExtrudeSpec = de(&ctx, &body)?;
            BatchItem::Op(Op::Extrude {
                name: s.name,
                profile: s
                    .profile
                    .rings()
                    .map_err(|e| format!("{ctx}: profile: {e}"))?,
                round: s.round,
                height: s.height,
                twist: s.twist,
                scale_top: s.scale_top,
                slices: s.slices,
                translation: s.translation,
                rotation: rot(&ctx, &s.rotation)?,
            })
        }
        "revolve" => {
            let s: RevolveSpec = de(&ctx, &body)?;
            BatchItem::Op(Op::Revolve {
                name: s.name,
                profile: s
                    .profile
                    .rings()
                    .map_err(|e| format!("{ctx}: profile: {e}"))?,
                round: s.round,
                degrees: s.degrees,
                segments: s.segments,
                translation: s.translation,
                rotation: rot(&ctx, &s.rotation)?,
            })
        }
        "sweep" => {
            let s: SweepSpec = de(&ctx, &body)?;
            BatchItem::Op(Op::Sweep {
                name: s.name,
                profile: expand_ring(s.profile).map_err(|e| format!("{ctx}: profile: {e}"))?,
                smooth: curve_smooth(&ctx, s.smooth, &s.curve)?,
                samples: s.samples,
                path: s.path,
                closed: s.closed,
                round: s.round,
                twist: s.twist,
                scale_end: s.scale_end,
            })
        }
        "loft" => {
            let s: LoftSpec = de(&ctx, &body)?;
            let on_path = !s.path.is_empty();
            let mut sections = Vec::with_capacity(s.sections.len());
            for (k, sec) in s.sections.into_iter().enumerate() {
                let c = format!("{ctx}: sections[{k}]");
                let at = match (sec.at, sec.y) {
                    (Some(_), Some(_)) => return Err(format!("{c}: give at or y, not both")),
                    (Some(a), None) => a,
                    (None, Some(y)) => [0.0, y, 0.0],
                    (None, None) if on_path => [0.0; 3],
                    (None, None) => {
                        return Err(format!(
                            "{c}: place the section with \"y\": h or \"at\": [x, y, z]"
                        ));
                    }
                };
                if on_path && (sec.at.is_some() || sec.y.is_some() || sec.rotation.is_some()) {
                    return Err(format!(
                        "{c}: on a path, sections are placed by t (0..1 along it), not at / y / rotation"
                    ));
                }
                if !on_path && sec.t.is_some() {
                    return Err(format!("{c}: t needs a path; use y or at"));
                }
                sections.push(crate::loft::LoftSection {
                    profile: expand_ring(sec.profile).map_err(|e| format!("{c}: profile: {e}"))?,
                    at,
                    t: sec.t,
                    rotation: rot(&c, &sec.rotation)?,
                    scale: match sec.scale {
                        None => [1.0, 1.0],
                        Some(ScaleArg::One(v)) => [v, v],
                        Some(ScaleArg::Two(v)) => v,
                    },
                });
            }
            if sections.len() < 2 {
                return Err(format!("{ctx}: sections: a loft needs at least 2"));
            }
            BatchItem::Op(Op::Loft {
                name: s.name,
                sections,
                path: s.path,
                smooth: s.smooth,
                samples: s.samples,
                steps: s.steps,
                round: s.round,
            })
        }
        "path_array" => {
            let s: PathArraySpec = de(&ctx, &body)?;
            if s.count == 0 && (s.spacing.is_nan() || s.spacing <= 0.0) {
                return Err(format!("{ctx}: give count (>= 1) or spacing (> 0, meters)"));
            }
            if s.count > 0 && s.spacing > 0.0 {
                return Err(format!("{ctx}: give count or spacing, not both"));
            }
            BatchItem::Op(Op::PathArray {
                name: s.name,
                input: s.input,
                smooth: curve_smooth(&ctx, s.smooth, &s.curve)?,
                samples: s.samples,
                path: s.path,
                closed: s.closed,
                count: s.count,
                spacing: s.spacing,
                align: s.align,
                roll_step: s.roll_step,
            })
        }
        "text" => {
            let s: TextSpec = de(&ctx, &body)?;
            let align = s.align.unwrap_or_else(|| "center".into());
            if !["center", "left", "right"].contains(&align.as_str()) {
                return Err(format!(
                    "{ctx}: align: center, left or right, got `{align}`"
                ));
            }
            BatchItem::Op(Op::Text {
                name: s.name,
                text: s.text,
                font: s.font,
                size: s.size,
                depth: s.depth.unwrap_or(s.size / 8.0),
                align,
                line_spacing: s.line_spacing.unwrap_or(1.0),
                letter_spacing: s.letter_spacing,
                translation: s.translation,
                rotation: rot(&ctx, &s.rotation)?,
            })
        }
        "lsystem" => {
            let s: LSystemSpec = de(&ctx, &body)?;
            BatchItem::Op(s.into_op(&ctx)?)
        }
        "terrain" => {
            let s: TerrainSpec = de(&ctx, &body)?;
            BatchItem::Op(s.into_op(&ctx)?)
        }
        "hull" => {
            let s: HullSpec = de(&ctx, &body)?;
            if s.inputs.is_empty() {
                return Err(format!("{ctx}: inputs: a hull needs at least one part"));
            }
            BatchItem::Op(Op::Hull {
                name: s.name,
                inputs: s.inputs,
            })
        }
        "blob" => {
            let s: BlobSpec = de(&ctx, &body)?;
            if s.inputs.is_empty() {
                return Err(format!("{ctx}: inputs: a blob needs at least one part"));
            }
            let op = s.op.unwrap_or_else(|| "union".into());
            if !["union", "subtract", "intersect"].contains(&op.as_str()) {
                return Err(format!(
                    "{ctx}: op: must be union, subtract or intersect, got `{op}`"
                ));
            }
            if s.blend.is_some_and(|b| b.is_nan() || b < 0.0) {
                return Err(format!("{ctx}: blend: must be >= 0 meters"));
            }
            let resolution = s.resolution.unwrap_or(0.0);
            if resolution.is_nan() || resolution < 0.0 {
                return Err(format!("{ctx}: resolution: must be > 0 meters"));
            }
            BatchItem::Op(Op::Blob {
                name: s.name,
                inputs: s.inputs,
                op,
                blend: s.blend,
                resolution,
            })
        }
        "split" => {
            let s: SplitSpec = de(&ctx, &body)?;
            let keep = s.keep.unwrap_or_else(|| "both".into());
            if !["both", "above", "below"].contains(&keep.as_str()) {
                return Err(format!(
                    "{ctx}: keep: must be both, above or below, got `{keep}`"
                ));
            }
            let normal = s.plane.normal.unwrap_or([0.0, 1.0, 0.0]);
            if normal.iter().all(|c| *c == 0.0) || normal.iter().any(|c| !c.is_finite()) {
                return Err(format!("{ctx}: plane.normal: must be a non-zero direction"));
            }
            if s.gap.is_nan() || s.gap < 0.0 {
                return Err(format!("{ctx}: gap: must be >= 0 meters"));
            }
            BatchItem::Op(Op::Split {
                name: s.name,
                input: s.input,
                normal,
                point: s.plane.point.unwrap_or([0.0; 3]),
                keep,
                gap: s.gap,
            })
        }
        "scatter" => {
            let s: ScatterSpec = de(&ctx, &body)?;
            BatchItem::Op(s.into_op(&ctx)?)
        }
        "round" => {
            let s: RoundSpec = de(&ctx, &body)?;
            if !["fillet", "chamfer", "smooth"].contains(&s.mode.as_str()) {
                return Err(format!(
                    "{ctx}: mode: must be fillet, chamfer or smooth, got `{}`",
                    s.mode
                ));
            }
            BatchItem::Op(Op::Round {
                name: s.name,
                input: s.input,
                mode: s.mode,
                radius: s.radius,
                segments: s.segments,
            })
        }
        "deform" => {
            let s: DeformSpec = de(&ctx, &body)?;
            let is_material = s.mode == "material";
            let def = crate::deform::Deform {
                mode: s.mode,
                axis: s.axis,
                degrees: s.degrees,
                toward: s.toward,
                scale_end: s.scale_end.map(|v| match v {
                    ScaleEndArg::Both(x) => [x, x],
                    ScaleEndArg::Each(a) => a,
                }),
                power: s.power,
                amount: s.amount,
                scale: if is_material { None } else { s.scale },
                octaves: s.octaves,
                seed: s.seed,
                noise: s.noise,
                detail: s.detail,
                pbr: match (s.pbr, is_material) {
                    (Some(m), true) => Some(crate::material::PbrRef {
                        material: m,
                        params: s.params.unwrap_or_default(),
                        scale: s.scale.unwrap_or(1.0),
                        graph: None,
                    }),
                    (Some(_), false) => {
                        return Err(format!("{ctx}: pbr: only for mode \"material\""));
                    }
                    (None, _) if s.params.is_some() => {
                        return Err(format!(
                            "{ctx}: params: only with mode \"material\" and a pbr"
                        ));
                    }
                    (None, _) => None,
                },
            };
            def.validate().map_err(|e| format!("{ctx}: {e}"))?;
            BatchItem::Op(Op::Deform {
                name: s.name,
                input: s.input,
                def: Box::new(def),
            })
        }
        "sdf" => {
            let s: SdfSpec = de(&ctx, &body)?;
            let shapes = s
                .shapes
                .into_iter()
                .enumerate()
                .map(|(i, c)| c.into_shape(&format!("shapes[{i}]")))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("{ctx}: {e}"))?;
            let def = crate::sdf::SdfDef {
                shapes,
                blend: s.blend,
                mirror: s.mirror,
                warps: s
                    .warps
                    .unwrap_or_default()
                    .into_iter()
                    .map(WarpSpec::into_deform)
                    .collect(),
                resolution: s.resolution,
            };
            def.validate().map_err(|e| format!("{ctx}: {e}"))?;
            BatchItem::Op(Op::Sdf {
                name: s.name,
                def: Box::new(def),
            })
        }
        "skin" => {
            let s: SkinSpec = de(&ctx, &body)?;
            let def = crate::sdf::SkinDef {
                chains: s
                    .chains
                    .into_iter()
                    .map(|c| {
                        c.into_iter()
                            .map(|p| match p {
                                ChainPtSpec::Joint(n) => crate::sdf::ChainPt::Joint(n),
                                ChainPtSpec::Point(q) => crate::sdf::ChainPt::Point(q),
                            })
                            .collect()
                    })
                    .collect(),
                joints: s.joints.unwrap_or_default(),
                smooth: s.smooth,
                blend: s.blend,
                mirror: s.mirror,
                warps: s
                    .warps
                    .unwrap_or_default()
                    .into_iter()
                    .map(WarpSpec::into_deform)
                    .collect(),
                resolution: s.resolution,
            };
            def.validate().map_err(|e| format!("{ctx}: {e}"))?;
            BatchItem::Op(Op::Skin {
                name: s.name,
                def: Box::new(def),
            })
        }
        "decimate" => {
            let s: DecimateSpec = de(&ctx, &body)?;
            let def = crate::decimate::Decimate {
                ratio: s.ratio,
                tris: s.tris,
                bake: s.bake,
                resolution: s.resolution,
                cage: s.cage,
                crease: s.crease,
            };
            def.validate().map_err(|e| format!("{ctx}: {e}"))?;
            BatchItem::Op(Op::Decimate {
                name: s.name,
                input: s.input,
                def: Box::new(def),
            })
        }
        "smooth" => {
            let s: SmoothSpec = de(&ctx, &body)?;
            let def = crate::smooth::Smooth {
                mode: s.mode,
                iterations: s.iterations,
                strength: s.strength,
                edge: s.edge,
                levels: s.levels,
                crease: s.crease,
            };
            def.validate().map_err(|e| format!("{ctx}: {e}"))?;
            BatchItem::Op(Op::Smooth {
                name: s.name,
                input: s.input,
                def: Box::new(def),
            })
        }
        "shell" => {
            let s: ShellSpec = de(&ctx, &body)?;
            if s.thickness.is_nan() || s.thickness <= 0.0 {
                return Err(format!("{ctx}: thickness: must be > 0 meters"));
            }
            let open = s.open.into_vec();
            for w in &open {
                if !["top", "bottom", "left", "right", "front", "back"].contains(&w.as_str()) {
                    return Err(format!(
                        "{ctx}: open: `{w}` is not a face; use top, bottom, left, right, front or back"
                    ));
                }
            }
            BatchItem::Op(Op::Shell {
                name: s.name,
                input: s.input,
                thickness: s.thickness,
                open,
            })
        }
        "group" => {
            let s: GroupSpec = de(&ctx, &body)?;
            if s.inputs.is_empty() {
                return Err(format!("{ctx}: inputs: a group needs at least one part"));
            }
            BatchItem::Op(Op::Group {
                name: s.name,
                inputs: s.inputs,
            })
        }
        "transform" => {
            let s: TransformSpec = de(&ctx, &body)?;
            BatchItem::Op(Op::Transform {
                name: s.name,
                input: s.input,
                translation: s.translation,
                rotation: rot(&ctx, &s.rotation)?,
                scale: s.scale,
                relative: s.relative,
                pivot: s.pivot,
            })
        }
        "collider" => {
            let s: ColliderSpec = de(&ctx, &body)?;
            if !matches!(s.shape.as_str(), "box" | "hull" | "mesh" | "none") {
                return Err(format!(
                    "{ctx}: shape: `{}` (one of box, hull, mesh, none)",
                    s.shape
                ));
            }
            let body_kind = s.body.unwrap_or_else(|| "static".into());
            if !matches!(body_kind.as_str(), "static" | "dynamic" | "kinematic") {
                return Err(format!(
                    "{ctx}: body: `{body_kind}` (one of static, dynamic, kinematic)"
                ));
            }
            if s.shape == "mesh" && body_kind == "dynamic" {
                return Err(format!(
                    "{ctx}: a dynamic body can't use a mesh collider (physics engines need convex shapes): use hull or box"
                ));
            }
            if s.parts.is_empty() {
                return Err(format!("{ctx}: parts: list at least one part or glob"));
            }
            BatchItem::Op(Op::Collider {
                name: s.name,
                parts: s.parts,
                shape: s.shape,
                body: body_kind,
                sensor: s.sensor,
                hidden: s.hidden,
            })
        }
        "skeleton" => {
            let s: SkeletonSpec = de(&ctx, &body)?;
            if s.bones.is_empty() && s.from.is_none() {
                return Err(format!(
                    "{ctx}: bones: needs at least one bone (or `from`: an lsystem or skin op to rig)"
                ));
            }
            if s.bone_length.is_some_and(|v| v.is_nan() || v <= 0.0) {
                return Err(format!("{ctx}: bone_length must be positive (meters)"));
            }
            if s.max_bones.is_some_and(|v| v == 0 || v > 256) {
                return Err(format!("{ctx}: max_bones must be 1..256"));
            }
            if let Some(m) = &s.mirror
                && m != "x"
            {
                return Err(format!("{ctx}: mirror `{m}`: only \"x\" is supported"));
            }
            let mut bones = Vec::new();
            for (n, b) in s.bones {
                if b.radius.is_some_and(|r| r.is_nan() || r <= 0.0) {
                    return Err(format!("{ctx}: bones.{n}.radius must be positive"));
                }
                if let crate::anim::PointRef::Anchor(a) = &b.head
                    && !a.contains(':')
                {
                    return Err(format!(
                        "{ctx}: bones.{n}.head `{a}`: an anchor names a part and words, like \"leg_l:top\""
                    ));
                }
                bones.push(crate::anim::BoneDef {
                    name: n,
                    parent: b.parent,
                    head: b.head,
                    tail: b.tail,
                    radius: b.radius,
                });
            }
            BatchItem::Op(Op::Skeleton {
                name: s.name,
                def: crate::anim::SkeletonDef {
                    bones,
                    mirror: s.mirror,
                    from: s.from,
                    bone_length: s.bone_length,
                    max_depth: s.max_depth,
                    max_bones: s.max_bones,
                },
            })
        }
        "bind" => {
            let s: BindSpec = de(&ctx, &body)?;
            let mode = s
                .mode
                .unwrap_or_else(|| if s.bone.is_some() { "rigid" } else { "smooth" }.into());
            match mode.as_str() {
                "rigid" if s.bone.is_none() => {
                    return Err(format!(
                        "{ctx}: rigid needs `bone` (the bone the parts ride)"
                    ));
                }
                "smooth" if s.bone.is_some() => {
                    return Err(format!(
                        "{ctx}: smooth picks bones itself: drop `bone` (or use mode rigid)"
                    ));
                }
                "auto" if s.bone.is_some() => {
                    return Err(format!("{ctx}: auto picks bones itself: drop `bone`"));
                }
                "rigid" | "smooth" | "auto" => {}
                other => {
                    return Err(format!("{ctx}: mode `{other}`: use rigid, smooth or auto"));
                }
            }
            if s.parts.is_empty() {
                return Err(format!("{ctx}: parts: list at least one part"));
            }
            BatchItem::Op(Op::Bind {
                name: s.name,
                def: crate::anim::BindDef {
                    skeleton: s.skeleton,
                    parts: s.parts,
                    bone: s.bone,
                    mode,
                    blend: s.blend,
                    paint: s
                        .paint
                        .into_iter()
                        .map(|p| crate::anim::Paint {
                            bone: p.bone,
                            min: p.region.min,
                            max: p.region.max,
                        })
                        .collect(),
                },
            })
        }
        "clip" => {
            let s: ClipSpec = de(&ctx, &body)?;
            if s.duration.is_nan() || s.duration <= 0.0 {
                return Err(format!("{ctx}: duration must be positive (seconds)"));
            }
            let default = match &s.interp {
                Some(i) => interp_of(&ctx, i)?,
                None => crate::anim::Interp::Smooth,
            };
            let mut tracks = std::collections::BTreeMap::new();
            for (bone, v) in &s.tracks {
                let Value::Object(o) = v else {
                    return Err(format!(
                        "{ctx}: tracks.{bone}: expected {{rotate / translate / scale: channel}}"
                    ));
                };
                let mut t = crate::anim::TrackDef::default();
                for (k, c) in o {
                    let cctx = format!("{ctx}: tracks.{bone}.{k}");
                    let ch = match k.as_str() {
                        "rotate" | "translate" | "scale" => {
                            channel(&cctx, k, c, s.duration, default)?
                        }
                        other => {
                            return Err(format!(
                                "{ctx}: tracks.{bone}: unknown channel `{other}` (rotate, translate, scale)"
                            ));
                        }
                    };
                    match k.as_str() {
                        "rotate" => t.rotate = Some(ch),
                        "translate" => t.translate = Some(ch),
                        _ => t.scale = Some(ch),
                    }
                }
                tracks.insert(bone.clone(), t);
            }
            BatchItem::Op(Op::Clip {
                name: s.name,
                def: crate::anim::ClipDef {
                    skeleton: s.skeleton,
                    duration: s.duration,
                    looping: s.looping.unwrap_or(true),
                    tracks,
                    mirror: s
                        .mirror
                        .into_iter()
                        .map(|(k, m)| {
                            (
                                k,
                                crate::anim::MirrorDef {
                                    from: m.from,
                                    phase: m.phase,
                                },
                            )
                        })
                        .collect(),
                },
            })
        }
        "sway" => {
            let s: SwaySpec = de(&ctx, &body)?;
            let kind = s.kind.as_deref().unwrap_or("wind");
            for (field, v) in [
                ("duration", s.duration),
                ("fps", s.fps),
                ("frequency", s.frequency),
                ("stiffness", s.stiffness),
            ] {
                if v.is_some_and(|v| v.is_nan() || v <= 0.0) {
                    return Err(format!("{ctx}: {field} must be positive"));
                }
            }
            for (field, v) in [
                ("strength", s.strength),
                ("gust", s.gust),
                ("flutter", s.flutter),
                ("damping", s.damping),
            ] {
                if v.is_some_and(|v| v.is_nan() || v < 0.0) {
                    return Err(format!("{ctx}: {field} must be 0 or more"));
                }
            }
            let d = modeler_anim::Wind::default();
            let wind = modeler_anim::Wind {
                direction: s.direction.unwrap_or(d.direction),
                strength: s.strength.unwrap_or(d.strength),
                gust: s.gust.unwrap_or(d.gust),
                flutter: s.flutter.unwrap_or(d.flutter),
                frequency: s.frequency.unwrap_or(d.frequency),
                seed: s.seed.unwrap_or(d.seed),
            };
            let sk = match kind {
                "wind" => {
                    if s.stiffness.is_some() || s.damping.is_some() || s.gravity.is_some() {
                        return Err(format!(
                            "{ctx}: stiffness / damping / gravity are for kind spring"
                        ));
                    }
                    modeler_anim::SwayKind::Wind(wind)
                }
                "spring" => {
                    let d = modeler_anim::Spring::default();
                    if s.flutter.is_some() {
                        return Err(format!("{ctx}: flutter is for kind wind"));
                    }
                    modeler_anim::SwayKind::Spring(modeler_anim::Spring {
                        stiffness: s.stiffness.unwrap_or(d.stiffness),
                        damping: s.damping.unwrap_or(d.damping),
                        gravity: s.gravity.unwrap_or(d.gravity),
                        wind: s.strength.is_some().then_some(modeler_anim::Wind {
                            flutter: 0.0,
                            ..wind
                        }),
                    })
                }
                other => return Err(format!("{ctx}: kind `{other}`: use wind or spring")),
            };
            BatchItem::Op(Op::Sway {
                name: s.name,
                def: crate::anim::SwayDef {
                    skeleton: s.skeleton,
                    over: s.over,
                    bones: s.bones,
                    duration: s.duration,
                    fps: s.fps,
                    motion: sk,
                },
            })
        }
        "marker" => {
            let s: MarkerSpec = de(&ctx, &body)?;
            if s.kind.trim().is_empty() {
                return Err(format!("{ctx}: kind must not be empty"));
            }
            if let Some(z) = s.size
                && z.iter().any(|v| v.is_nan() || *v <= 0.0)
            {
                return Err(format!("{ctx}: size must be positive on every axis"));
            }
            BatchItem::Op(Op::Marker {
                name: s.name,
                kind: s.kind,
                translation: s.translation,
                yaw: s.yaw,
                size: s.size,
                data: s.data,
                attach: s.attach,
                preview: s.preview,
                components: {
                    crate::scene::check_components(&s.components)
                        .map_err(|e| format!("{ctx}: {e}"))?;
                    s.components
                },
            })
        }
        "decal" => {
            let s: DecalSpec = de(&ctx, &body)?;
            let size = match s.size.as_slice() {
                [w, h] => [*w, *h, w.min(*h) * 0.5],
                [w, h, d] => [*w, *h, *d],
                _ => {
                    return Err(format!(
                        "{ctx}: size is [width, height] or [width, height, depth]"
                    ));
                }
            };
            if size.iter().any(|v| !(*v > 0.0 && v.is_finite())) {
                return Err(format!("{ctx}: size must be positive"));
            }
            let kind = s.kind.unwrap_or_else(|| "clustered".into());
            if !matches!(kind.as_str(), "clustered" | "forward") {
                return Err(format!("{ctx}: kind: `{kind}`: use clustered or forward"));
            }
            if let Some(t) = &s.toward
                && crate::decal::toward_dir(t).is_none()
            {
                return Err(format!(
                    "{ctx}: toward: `{t}`: use down, up, left, right, back or front"
                ));
            }
            if s.on.is_none() && (s.at.is_some() || s.toward.is_some()) {
                return Err(format!(
                    "{ctx}: at / toward go with `on` (the part to stick to)"
                ));
            }
            if s.on.is_some() && (s.direction.is_some() || s.attach.is_some()) {
                return Err(format!(
                    "{ctx}: with `on` the decal follows that part and projects `toward`; drop direction / attach"
                ));
            }
            if let Some(d) = s.direction
                && d.iter().map(|v| v * v).sum::<f32>() < 1e-12
            {
                return Err(format!("{ctx}: direction must not be zero"));
            }
            BatchItem::Op(Op::Decal {
                name: s.name,
                pbr: crate::material::PbrRef {
                    material: s.pbr,
                    params: s.params.unwrap_or_default(),
                    scale: 1.0,
                    graph: None,
                },
                size,
                translation: s.translation,
                direction: s.direction,
                roll: s.roll,
                on: s.on,
                at: s.at,
                toward: s.toward,
                attach: s.attach,
                kind,
            })
        }
        "light" => {
            let s: LightSpec = de(&ctx, &body)?;
            if !matches!(s.kind.as_str(), "point" | "spot" | "directional") {
                return Err(format!(
                    "{ctx}: kind: `{}` is not a light (point, spot or directional)",
                    s.kind
                ));
            }
            if s.intensity.is_some_and(|v| !(v >= 0.0 && v.is_finite())) {
                return Err(format!("{ctx}: intensity must be >= 0"));
            }
            if let Some(d) = s.direction
                && d.iter().map(|v| v * v).sum::<f32>() < 1e-12
            {
                return Err(format!("{ctx}: direction must not be zero"));
            }
            let color = match &s.color {
                Some(c) => parse_color(c).map_err(|e| format!("{ctx}: color: {e}"))?,
                None => [1.0; 4],
            };
            BatchItem::Op(Op::Light {
                name: s.name,
                kind: s.kind,
                color,
                intensity: s.intensity,
                range: s.range,
                translation: s.translation,
                direction: s.direction,
                inner_degrees: s.inner_degrees,
                outer_degrees: s.outer_degrees,
                shadows: s.shadows,
                attach: s.attach,
            })
        }
        "move" => {
            let s: MoveSpec = de(&ctx, &body)?;
            let rotation = match (rot(&ctx, &s.rotation)?, s.axis, s.degrees) {
                (Some(_), Some(_), _) | (Some(_), _, Some(_)) => {
                    return Err(format!(
                        "{ctx}: give rotation (a quaternion) or axis + degrees, not both"
                    ));
                }
                (Some(q), None, None) => Some(q),
                (None, Some(a), Some(d)) => {
                    let a = crate::Vec3::from(a);
                    if a.length() < 1e-9 {
                        return Err(format!("{ctx}: axis is zero"));
                    }
                    Some(crate::Quat::from_axis_angle(a.normalize(), d.to_radians()).to_array())
                }
                (None, None, None) => None,
                _ => return Err(format!("{ctx}: axis and degrees go together")),
            };
            if s.parts.is_empty() {
                return Err(format!(
                    "{ctx}: parts is empty (use [\"*\"] for everything)"
                ));
            }
            BatchItem::Move {
                name: s.name,
                parts: s.parts,
                translation: s.translation,
                rotation,
                pivot: s.pivot,
                on: None,
                at: None,
                gap: None,
                toward: None,
                each: false,
            }
        }
        "repeat" => {
            return Err(format!(
                "{ctx}: repeat is expanded by apply_ops before parsing; nest it in an apply_ops batch"
            ));
        }
        "place" => {
            let s: PlaceSpec = de(&ctx, &body)?;
            if s.parts.is_empty() {
                return Err(format!("{ctx}: parts is empty"));
            }
            if s.each && s.at.is_some() {
                return Err(format!(
                    "{ctx}: each and at don't mix: with each, every part keeps its own footprint"
                ));
            }
            if let Some(t) = &s.toward
                && !["down", "up", "left", "right", "back", "front"].contains(&t.as_str())
            {
                return Err(format!(
                    "{ctx}: toward: `{t}` is not a direction; use down, up, left, right, back or front"
                ));
            }
            BatchItem::Move {
                name: s.name,
                parts: s.parts,
                translation: None,
                rotation: None,
                pivot: None,
                on: Some(s.on),
                at: s.at,
                gap: s.gap,
                toward: s.toward.filter(|t| t != "down"),
                each: s.each,
            }
        }
        "boolean" => {
            let s: BooleanSpec = de(&ctx, &body)?;
            BatchItem::Op(Op::Boolean {
                name: s.name,
                op: s.op,
                a: s.a,
                b: s.b,
            })
        }
        "radial_array" => {
            let s: RadialArraySpec = de(&ctx, &body)?;
            BatchItem::Op(Op::RadialArray {
                name: s.name,
                input: s.input,
                count: s.count,
                center: s.center,
                degrees: s.degrees,
                start_degrees: s.start_degrees,
                axis: s.axis,
                include_source: s.include_source,
            })
        }
        "linear_array" => {
            let s: LinearArraySpec = de(&ctx, &body)?;
            BatchItem::Op(Op::LinearArray {
                name: s.name,
                input: s.input,
                count: s.count,
                offset: s.offset,
                local: s.local,
            })
        }
        "mirror" => {
            let s: MirrorSpec = de(&ctx, &body)?;
            BatchItem::Op(Op::Mirror {
                name: s.name,
                input: s.input,
                axis: s.axis,
                offset: s.offset,
            })
        }
        "material" => {
            let s: MaterialSpec = de(&ctx, &body)?;
            if s.pbr.is_none() && (s.params.is_some() || s.scale.is_some()) {
                return Err(format!(
                    "{ctx}: params / scale apply to a pbr material; add \"pbr\": \"<material>\""
                ));
            }
            if let Some(k) = s.scale
                && !(k > 0.0 && k.is_finite())
            {
                return Err(format!("{ctx}: scale: {k}: give a factor > 0"));
            }
            BatchItem::Op(Op::Material {
                input: s.input.unwrap_or_else(|| s.name.clone()),
                name: s.name,
                base_color: s.base_color.map(|c| c.0),
                roughness: s.roughness,
                metalness: s.metalness,
                pbr: s.pbr.map(|m| crate::material::PbrRef {
                    material: m,
                    params: s.params.unwrap_or_default(),
                    scale: s.scale.unwrap_or(1.0),
                    graph: None,
                }),
                flat: s.flat,
            })
        }
        "hide_part" => BatchItem::Hide {
            part: de::<PartSpec>(&ctx, &body)?.part,
        },
        "show_part" => BatchItem::Show {
            part: de::<PartSpec>(&ctx, &body)?.part,
        },
        "rename" => {
            let s: RenameSpec = de(&ctx, &body)?;
            BatchItem::Rename {
                part: s.part,
                to: s.to,
            }
        }
        "delete" => BatchItem::Delete {
            name: de::<DeleteSpec>(&ctx, &body)?.name,
        },
        "tri_mesh" => {
            let s: TriMeshSpec = de(&ctx, &body)?;
            BatchItem::TriMesh {
                name: s.name,
                verts: s.verts,
                indices: s.indices,
                normals: s.normals,
                uvs: s.uvs,
            }
        }
        "uvs" => {
            let s: UvsSpec = de(&ctx, &body)?;
            BatchItem::Uvs {
                part: s.part,
                projection: s.projection,
            }
        }
        "prefab" => {
            let s: PrefabSpec = de(&ctx, &body)?;
            let model =
                crate::catalog::prefab_instance(&s.prefab).map_err(|e| format!("{ctx}: {e}"))?;
            let parts: Vec<String> = model
                .parts
                .iter()
                .filter(|p| p.visible)
                .map(|p| p.name.clone())
                .collect();
            if parts.is_empty() {
                return Err(format!("{ctx}: prefab `{}` has no visible parts", s.prefab));
            }
            let digest = {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                serde_json::to_string(&model.ops)
                    .unwrap_or_default()
                    .hash(&mut h);
                parts.hash(&mut h);
                h.finish()
            };
            BatchItem::Op(Op::Prefab {
                name: s.name,
                prefab: s.prefab,
                translation: s.translation,
                rotation: rot(&ctx, &s.rotation)?,
                scale: s.scale,
                parts,
                digest,
                style: match s.style {
                    None => None,
                    Some(v @ serde_json::Value::Object(_)) => Some(v),
                    Some(serde_json::Value::String(m)) if m == "scene" => {
                        Some(serde_json::Value::String(m))
                    }
                    Some(other) => {
                        return Err(format!(
                            "{ctx}: style: \"scene\" or a token object, got {other}"
                        ));
                    }
                },
                restyle: None,
                components: {
                    crate::scene::check_components(&s.components)
                        .map_err(|e| format!("{ctx}: {e}"))?;
                    s.components
                },
            })
        }
        other => {
            return Err(format!(
                "op #{i}: unknown type `{other}` (one of: {})",
                OP_TYPES.join(", ")
            ));
        }
    })
}

/// A ready-to-send example item for each type, for `describe_op`.
pub fn example(ty: &str) -> Option<Value> {
    use serde_json::json;
    Some(match ty {
        "primitive" => {
            json!({"type": "primitive", "name": "leg", "kind": "cylinder", "size": {"radius": 0.05, "height": 0.8}, "translation": [0.4, 0.4, 0.3]})
        }
        "extrude" => {
            json!({"type": "extrude", "name": "flag", "profile": [[0, 0], [0.6, 0.05], [0.6, 0.35], [0, 0.4]], "height": 0.02, "rotation": [-0.707, 0, 0, 0.707], "translation": [0.03, 7.6, 0.01]})
        }
        "revolve" => {
            json!({"type": "revolve", "name": "vase", "profile": [[0, 0], [0.3, 0], [0.4, 0.3], [0.15, 0.7], [0.2, 0.8], [0, 0.8]]})
        }
        "sweep" => {
            json!({"type": "sweep", "name": "handle", "profile": [{"arc": {"center": [0, 0], "radius": 0.012, "from": 0, "to": 360}}], "path": [[0, 0.9, 0], [0, 0.95, 0.05], [0, 1.05, 0.05], [0, 1.1, 0]]})
        }
        "text" => {
            json!({"type": "text", "name": "exit_sign", "text": "EXIT", "font": "bold", "size": 0.15, "depth": 0.02, "translation": [0, 2.2, 0.01]})
        }
        "loft" => {
            json!({"type": "loft", "name": "bottle", "smooth": true, "sections": [
                {"y": 0, "profile": [{"arc": {"center": [0, 0], "radius": 0.035, "from": 0, "to": 360}}]},
                {"y": 0.16, "profile": [{"arc": {"center": [0, 0], "radius": 0.037, "from": 0, "to": 360}}]},
                {"y": 0.22, "profile": [{"arc": {"center": [0, 0], "radius": 0.014, "from": 0, "to": 360}}]},
                {"y": 0.28, "profile": [{"arc": {"center": [0, 0], "radius": 0.013, "from": 0, "to": 360}}]}]})
        }
        "path_array" => {
            json!({"type": "path_array", "name": "posts", "input": "post", "path": [[0, 0, 0], [2, 0, 1], [4, 0, 0]], "smooth": true, "spacing": 0.8})
        }
        "lsystem" => {
            json!({"type": "lsystem", "name": "oak", "preset": "tree", "seed": 3, "translation": [2, 0, 0]})
        }
        "terrain" => {
            json!({"type": "terrain", "name": "ground", "size": [60, 60], "height": 6, "scale": 20, "style": "hills", "seed": 1, "flatten": [{"at": [0, 0], "radius": 6, "falloff": 4}], "paths": [{"points": [[-30, -10], [0, -4], [30, 8]], "width": 3, "depth": 0.3}]})
        }
        "hull" => {
            json!({"type": "hull", "name": "pill", "inputs": ["ball_a", "ball_b"]})
        }
        "blob" => {
            json!({"type": "blob", "name": "creature", "inputs": ["body", "head", "arm_l", "arm_r"], "blend": 0.04})
        }
        "split" => {
            json!({"type": "split", "name": "vase", "input": "vase_raw", "plane": {"normal": [0, 1, 0], "point": [0, 0.15, 0]}, "gap": 0.02})
        }
        "scatter" => {
            json!({"type": "scatter", "name": "rocks", "input": ["rock_a", "rock_b"], "on": "ground", "count": 40, "seed": 2, "scale": [0.6, 1.4], "rotate": "random", "align": 0.5, "max_slope": 35})
        }
        "round" => {
            json!({"type": "round", "name": "table_top_r", "input": "table_top", "mode": "fillet", "radius": 0.01})
        }
        "deform" => {
            json!({"type": "deform", "name": "rock", "input": "rock_raw", "mode": "displace", "amount": 0.08, "scale": 0.4, "seed": 7})
        }
        "sdf" => {
            json!({"type": "sdf", "name": "critter", "mirror": "x", "blend": 0.03, "shapes": [
                {"shape": "ellipsoid", "size": [0.12, 0.1, 0.16], "at": [0, 0.14, 0]},
                {"shape": "sphere", "radius": 0.07, "at": [0, 0.24, 0.14]},
                {"shape": "capsule", "from": [0.06, 0.1, 0.08], "to": [0.09, 0.0, 0.1], "radius": [0.03, 0.022]},
                {"shape": "sphere", "radius": 0.02, "at": [0.03, 0.27, 0.2], "op": "subtract", "blend": 0.01}
            ]})
        }
        "skin" => {
            json!({"type": "skin", "name": "octo", "joints": {"body": [0, 0.25, 0, 0.12]}, "chains": [
                ["body", [0.16, 0.12, 0, 0.04], [0.32, 0.03, 0.05, 0.025], [0.45, 0.02, 0.15, 0.01]],
                ["body", [0, 0.42, 0, 0.1]]
            ], "mirror": "x"})
        }
        "decimate" => {
            json!({"type": "decimate", "name": "statue", "input": "statue_hi", "ratio": 0.05, "bake": true})
        }
        "smooth" => {
            json!({"type": "smooth", "name": "pebble", "input": "pebble_cage", "mode": "subdiv", "levels": 3})
        }
        "shell" => {
            json!({"type": "shell", "name": "carcass", "input": "carcass_raw", "thickness": 0.018, "open": "front"})
        }
        "group" => {
            json!({"type": "group", "name": "tile_row", "inputs": ["tile", "tile_1", "tile_2", "tile_3"]})
        }
        "transform" => {
            json!({"type": "transform", "name": "leg_raised", "input": "leg", "translation": [0, 0.1, 0], "relative": true})
        }
        "repeat" => {
            json!({"type": "repeat", "var": "r", "count": 4, "items": [
                {"type": "primitive", "name": "key{r}", "kind": "box", "size": [0.012, 0.004, 0.012], "translation": ["=$r * 0.019", 0.1, 0]}
            ]})
        }
        "place" => {
            json!({"type": "place", "name": "mug_on_desk", "parts": ["mug"], "on": "desk_top", "at": [-0.4, 0.1]})
        }
        "move" => {
            json!({"type": "move", "name": "hinge", "parts": ["lid"], "axis": [1, 0, 0], "degrees": -20, "pivot": "lid:bottom-back"})
        }
        "boolean" => {
            json!({"type": "boolean", "name": "wall_cut", "op": "difference", "a": "wall", "b": "door_cutter"})
        }
        "radial_array" => {
            json!({"type": "radial_array", "name": "spokes", "input": "spoke", "count": 6, "axis": "z", "center": [0, 1, 0]})
        }
        "linear_array" => {
            json!({"type": "linear_array", "name": "posts", "input": "post", "count": 9, "offset": [1, 0, 0]})
        }
        "mirror" => {
            json!({"type": "mirror", "name": "arm_r", "input": "arm", "axis": "x", "offset": [0, 0, 0]})
        }
        "material" => {
            json!({"type": "material", "name": "leg", "color": "#8b5a2b", "roughness": 0.8})
        }
        "hide_part" => json!({"type": "hide_part", "part": "door_cutter"}),
        "show_part" => json!({"type": "show_part", "part": "door_cutter"}),
        "rename" => json!({"type": "rename", "part": "wall_cut", "to": "wall"}),
        "collider" => {
            json!({"type": "collider", "name": "walls_solid", "parts": ["wall*", "floor"], "shape": "box"})
        }
        "skeleton" => json!({"type": "skeleton", "name": "rig", "mirror": "x", "bones": {
            "hip": {"head": [0, 0.5, 0], "tail": [0, 0.6, 0], "radius": 0.08},
            "leg_l": {"parent": "hip", "head": [0.1, 0.5, 0], "tail": [0.1, 0.05, 0], "radius": 0.04}}}),
        "bind" => json!({"type": "bind", "name": "body_skin", "skeleton": "rig", "parts": ["body"],
            "mode": "smooth", "blend": 0.04}),
        "clip" => json!({"type": "clip", "name": "walk", "skeleton": "rig", "duration": 1.0,
            "tracks": {"leg_l": {"rotate": [[0, {"x": 25}], [0.5, {"x": -25}], [1, {"x": 25}]]},
                       "hip": {"translate": {"wave": {"axis": "y", "amount": 0.02, "period": 0.5}}}},
            "mirror": {"leg_r": {"from": "leg_l", "phase": 0.5}}}),
        "sway" => json!({"type": "sway", "name": "wind", "skeleton": "oak_rig", "kind": "wind",
            "direction": [1, 0, 0.3], "strength": 0.6, "gust": 0.4, "flutter": 0.5, "duration": 4}),
        "marker" => json!({"type": "marker", "name": "spawn_1", "kind": "spawn",
            "translation": [0, 0, 2], "yaw": 180, "data": {"team": 1}}),
        "decal" => json!({"type": "decal", "name": "tag", "pbr": "graffiti",
            "size": [1.2, 0.8], "on": "wall_back", "toward": "back", "at": [0.5, 1.4]}),
        "light" => json!({"type": "light", "name": "lamp_bulb", "kind": "point",
            "translation": [0, 1.4, 0], "color": "#ffd8a8", "intensity": 800, "attach": "lamp_shade"}),
        "delete" => json!({"type": "delete", "name": "leg_raised"}),
        "tri_mesh" => {
            json!({"type": "tri_mesh", "name": "tri", "verts": [[0, 0, 0], [1, 0, 0], [0, 1, 0]], "indices": [0, 1, 2]})
        }
        "uvs" => json!({"type": "uvs", "part": "wall", "projection": "box"}),
        "prefab" => {
            json!({"type": "prefab", "name": "lamp1", "prefab": "lamp", "translation": [2, 0, 0]})
        }
        _ => return None,
    })
}

/// `describe_op`: with a type, its example and JSON schema; without, one
/// example line per type.
pub fn describe_type(ty: Option<&str>) -> Result<String, String> {
    if let Some(t) = ty {
        if t == "nodes" {
            return crate::material::node::describe(None);
        }
        if let Some(n) = t.strip_prefix("node:") {
            return crate::material::node::describe(Some(n));
        }
    }
    let Some(ty) = ty else {
        let mut out = vec![
            "apply_ops item types (describe_op with a kind for its full schema; \"nodes\" lists material_ops node types, \"node:<type>\" gives one). Values may be \"$param\" or \"=expr\".".to_string(),
        ];
        for t in OP_TYPES {
            if let Some(e) = example(t) {
                out.push(format!("- {t}: {e}"));
            }
        }
        return Ok(out.join("\n"));
    };
    let schema = match ty {
        "primitive" => schemars::schema_for!(PrimitiveSpec),
        "extrude" => schemars::schema_for!(ExtrudeSpec),
        "revolve" => schemars::schema_for!(RevolveSpec),
        "group" => schemars::schema_for!(GroupSpec),
        "sweep" => schemars::schema_for!(SweepSpec),
        "loft" => schemars::schema_for!(LoftSpec),
        "path_array" => schemars::schema_for!(PathArraySpec),
        "lsystem" => schemars::schema_for!(LSystemSpec),
        "text" => schemars::schema_for!(TextSpec),
        "terrain" => schemars::schema_for!(TerrainSpec),
        "hull" => schemars::schema_for!(HullSpec),
        "blob" => schemars::schema_for!(BlobSpec),
        "split" => schemars::schema_for!(SplitSpec),
        "scatter" => schemars::schema_for!(ScatterSpec),
        "round" => schemars::schema_for!(RoundSpec),
        "deform" => schemars::schema_for!(DeformSpec),
        "sdf" => schemars::schema_for!(SdfSpec),
        "skin" => schemars::schema_for!(SkinSpec),
        "smooth" => schemars::schema_for!(SmoothSpec),
        "decimate" => schemars::schema_for!(DecimateSpec),
        "shell" => schemars::schema_for!(ShellSpec),
        "transform" => schemars::schema_for!(TransformSpec),
        "move" => schemars::schema_for!(MoveSpec),
        "repeat" => schemars::schema_for!(RepeatSpec),
        "light" => schemars::schema_for!(LightSpec),
        "decal" => schemars::schema_for!(DecalSpec),
        "collider" => schemars::schema_for!(ColliderSpec),
        "marker" => schemars::schema_for!(MarkerSpec),
        "skeleton" => schemars::schema_for!(SkeletonSpec),
        "bind" => schemars::schema_for!(BindSpec),
        "clip" => schemars::schema_for!(ClipSpec),
        "sway" => schemars::schema_for!(SwaySpec),
        "place" => schemars::schema_for!(PlaceSpec),
        "boolean" => schemars::schema_for!(BooleanSpec),
        "radial_array" => schemars::schema_for!(RadialArraySpec),
        "linear_array" => schemars::schema_for!(LinearArraySpec),
        "mirror" => schemars::schema_for!(MirrorSpec),
        "material" => schemars::schema_for!(MaterialSpec),
        "hide_part" | "show_part" => schemars::schema_for!(PartSpec),
        "rename" => schemars::schema_for!(RenameSpec),
        "delete" => schemars::schema_for!(DeleteSpec),
        "tri_mesh" => schemars::schema_for!(TriMeshSpec),
        "uvs" => schemars::schema_for!(UvsSpec),
        "prefab" => schemars::schema_for!(PrefabSpec),
        other => {
            return Err(format!(
                "unknown item type `{other}` (one of: {})",
                OP_TYPES.join(", ")
            ));
        }
    };
    let example = example(ty).unwrap_or(Value::Null);
    Ok(format!(
        "example:\n{}\n\nschema (plus \"type\": \"{ty}\"):\n{}",
        serde_json::to_string_pretty(&example).unwrap_or_default(),
        serde_json::to_string_pretty(&schema).unwrap_or_default()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn one(v: Value) -> Result<BatchItem, String> {
        parse_item(3, &v)
    }

    #[test]
    fn rotation_forms_agree() {
        let rot = |r: Value| -> [f32; 4] {
            let item = one(
                json!({"type": "primitive", "name": "p", "kind": "box", "size": 1, "rotation": r}),
            )
            .unwrap_or_else(|e| panic!("{e}"));
            let BatchItem::Op(Op::Primitive { rotation, .. }) = item else {
                panic!("not a primitive");
            };
            rotation.unwrap_or_else(|| panic!("no rotation"))
        };
        let s = (-10.0f32).to_radians().sin();
        let c = (-10.0f32).to_radians().cos();
        let quat = rot(json!([s, 0, 0, c]));
        let axis = rot(json!({"axis": [2, 0, 0], "degrees": -20}));
        let euler = rot(json!({"x": -20}));
        for (a, b) in [(quat, axis), (quat, euler)] {
            for k in 0..4 {
                assert!((a[k] - b[k]).abs() < 1e-5, "{a:?} vs {b:?}");
            }
        }
        // Euler applies x, then y, then z about the world axes.
        let e = rot(json!({"x": 90, "y": 90}));
        let q = crate::quat_of(e);
        let v = q * bevy::math::Vec3::Y;
        assert!((v - bevy::math::Vec3::X).length() < 1e-5, "{v:?}");
        assert!(one(json!({"type": "primitive", "name": "p", "kind": "box", "size": 1, "rotation": {"axis": [0, 0, 0], "degrees": 5}})).is_err());
        assert!(one(json!({"type": "primitive", "name": "p", "kind": "box", "size": 1, "rotation": {"x": 5, "w": 1}})).is_err());
    }

    #[test]
    fn errors_name_the_item_and_field() {
        let Err(e) =
            one(json!({"type": "primitive", "name": "roof", "kind": "cone", "size": [1.0]}))
        else {
            panic!("short cone size should fail");
        };
        assert!(e.starts_with("op #3 (primitive \"roof\"): size:"), "{e}");
        let Err(e) = one(
            json!({"type": "primitive", "name": "r", "kind": "box", "size": 1, "translation": [0, 0]}),
        ) else {
            panic!("short translation should fail");
        };
        assert!(e.contains("translation"), "{e}");
        let Err(e) = one(
            json!({"type": "primitive", "name": "r", "kind": "box", "size": 1, "positon": [0, 0, 0]}),
        ) else {
            panic!("typo field should fail");
        };
        assert!(e.contains("positon"), "{e}");
        let Err(e) = one(json!({"type": "spin", "name": "x"})) else {
            panic!("unknown type should fail");
        };
        assert!(e.contains("unknown type `spin`"), "{e}");
    }

    #[test]
    fn lenient_sizes_and_colors() {
        let size = |k, v| resolve_size(k, &v).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(size(PrimitiveKind::Sphere, json!(0.5)), [0.5, 0.0, 0.0]);
        assert_eq!(size(PrimitiveKind::Box, json!(2)), [2.0, 2.0, 2.0]);
        assert_eq!(
            size(PrimitiveKind::Cylinder, json!([0.3, 2])),
            [0.3, 2.0, 0.0]
        );
        assert_eq!(
            size(PrimitiveKind::Cylinder, json!({"radius": 0.3, "height": 2})),
            [0.3, 2.0, 0.0]
        );
        assert_eq!(
            size(
                PrimitiveKind::Box,
                json!({"width": 1, "height": 2, "depth": 3})
            ),
            [1.0, 2.0, 3.0]
        );
        assert!(resolve_size(PrimitiveKind::Cylinder, &json!(1)).is_err());
        assert_eq!(parse_color(&json!([1, 0, 0])), Ok([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(parse_color(&json!([255, 0, 0])), Ok([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(parse_color(&json!("#ff0000")), Ok([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(parse_color(&json!("#f00")), Ok([1.0, 0.0, 0.0, 1.0]));
        // Arcs expand to points; a full circle has no duplicate seam point.
        let ring = expand_ring(
            serde_json::from_value(json!([{"arc": {"center": [0, 0], "radius": 1, "from": 0, "to": 360, "segments": 8}}]))
                .unwrap_or_else(|e| panic!("{e}")),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(ring.len(), 8);
        let d = expand_ring(
            serde_json::from_value(json!([[1, 0], {"arc": {"center": [0, 0], "radius": 1, "from": 0, "to": 90, "segments": 2}}, [0, 0]]))
                .unwrap_or_else(|e| panic!("{e}")),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(d.len(), 4, "{d:?}");
        assert!(parse_color(&json!([1, 0])).is_err());
    }

    #[test]
    fn material_defaults_to_in_place_and_radial_includes_source() {
        let Ok(BatchItem::Op(Op::Material { name, input, .. })) =
            one(json!({"type": "material", "name": "leg", "color": [1, 0, 0]}))
        else {
            panic!("material should parse");
        };
        assert_eq!(name, input);
        let Ok(BatchItem::Op(Op::RadialArray { include_source, .. })) =
            one(json!({"type": "radial_array", "name": "b", "input": "a", "count": 4}))
        else {
            panic!("radial should parse");
        };
        assert!(include_source);
    }

    #[test]
    fn friendly_aliases_parse() {
        let ok = |v: serde_json::Value| {
            parse_item(0, &v).unwrap_or_else(|e| panic!("{v}: {e}"));
        };
        ok(serde_json::json!({"type": "hide_part", "name": "ground"}));
        ok(serde_json::json!({"type": "lsystem", "name": "a", "axiom": "FFL", "iterations": 0}));
        let a: crate::anim::check::AnimationSpec = serde_json::from_value(
            serde_json::json!({"clip": "w", "stretch": true, "floor": true}),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!((a.stretch, a.floor), (Some(2.0), Some(0.0)));
    }

    #[test]
    fn every_type_has_a_parseable_example() {
        for t in OP_TYPES {
            let e = example(t).unwrap_or_else(|| panic!("no example for {t}"));
            if *t == "prefab" {
                continue; // needs a catalog entry
            }
            let none = crate::params::Params::default();
            for (item, _) in expand_repeats(&[e], &none).unwrap_or_else(|err| panic!("{t}: {err}"))
            {
                let item =
                    crate::params::resolve(&item, &none).unwrap_or_else(|err| panic!("{t}: {err}"));
                parse_item(0, &item).unwrap_or_else(|err| panic!("{t}: {err}"));
            }
            describe_type(Some(t)).unwrap_or_else(|err| panic!("{t}: {err}"));
        }
        assert!(describe_type(None).is_ok_and(|s| s.contains("radial_array")));
    }
}
