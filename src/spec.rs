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
    /// Box, cylinder and wedge only: round every edge (a cylinder's rims) by
    /// this many meters, keeping the outer size. A closed solid, ready for
    /// booleans. Style defaults can fill it: {"primitive": {"round": "$bevel"}}.
    #[serde(default)]
    round: f32,
    /// Arc segments per rounded edge (default 4).
    #[serde(default)]
    segments: u32,
    /// Plane only: make it ONE-sided, a single quad (2 triangles) whose
    /// front faces "up", "down", "+x", "-x", "+z" or "-z" (back faces cull).
    /// size is then [width, height] across the face: [x, z] for up/down,
    /// [z, y] for +x/-x, [x, y] for +z/-z. Without it a plane is a
    /// double-sided [x, z] sheet. Floors, ceilings and walls of tiles.
    #[serde(default)]
    facing: Option<crate::Facing>,
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
    Svg(SvgProfile),
}

/// A profile drawn in SVG: {"svg": "<svg ...>" or "<path d=...>", "size"?:
/// [w, h] meters (or the width alone), "height"?: meters, "center"?: [a,
/// b], "flip_y"?: true, "id"?: element id}. Every filled shape (strokes as
/// outlines, text as glyphs) becomes rings, holes kept (even-odd); the
/// content's bounds are fitted to size / height (default 1 unit = 1 mm)
/// and centred on `center` (default [0, 0]). SVG's y runs down: flip_y
/// (default) makes the drawing's up the profile's +b.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SvgProfile {
    svg: String,
    #[serde(default)]
    size: Option<ScaleArg>,
    #[serde(default)]
    height: Option<f32>,
    #[serde(default)]
    center: Option<[f32; 2]>,
    #[serde(default)]
    flip_y: Option<bool>,
    #[serde(default)]
    id: Option<String>,
}

impl SvgProfile {
    fn rings(&self) -> Result<Vec<Vec<[f32; 2]>>, String> {
        let (size, width_only) = match self.size {
            Some(ScaleArg::One(w)) => (Some([w, w]), true),
            Some(ScaleArg::Two(s)) => (Some(s), false),
            None => (None, false),
        };
        crate::svg::profile_rings(
            &self.svg,
            self.id.as_deref(),
            size,
            width_only,
            self.height,
            self.center.unwrap_or([0.0, 0.0]),
            self.flip_y.unwrap_or(true),
        )
        .map_err(|e| format!("svg: {e}"))
    }
}

/// One ring: points / arcs, or an svg profile (its largest ring).
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum RingArg {
    Ring(Vec<RingItem>),
    Svg(SvgProfile),
}

fn ring_area(r: &[[f32; 2]]) -> f32 {
    let mut a = 0.0;
    for i in 0..r.len() {
        let (p, q) = (r[i], r[(i + 1) % r.len()]);
        a += p[0] * q[1] - q[0] * p[1];
    }
    (a * 0.5).abs()
}

fn expand_ring_arg(a: RingArg) -> Result<Vec<[f32; 2]>, String> {
    match a {
        RingArg::Ring(r) => expand_ring(r),
        RingArg::Svg(s) => {
            let rings = s.rings()?;
            rings
                .into_iter()
                .max_by(|a, b| ring_area(a).total_cmp(&ring_area(b)))
                .ok_or_else(|| "svg: no rings".to_string())
        }
    }
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
            ProfileArg::Svg(s) => s.rings(),
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
    /// "radius": 0.02, "from": 0, "to": 360}}]. Or an svg profile (its
    /// largest ring).
    profile: RingArg,
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
    /// path, a is along the path normal (up) and b across, like sweep. Or
    /// an svg profile (its largest ring).
    profile: RingArg,
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
    /// A cage for `smooth subdiv`: keep each section's own points (every
    /// section the same count, no resampling, `steps` 1 unless given) and
    /// hand the walls to subdiv as quads and each cap as one polygon, so a
    /// curved cage subdivides without the lumps of a triangulated one.
    #[serde(default)]
    quads: bool,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PanelLineSpec {
    /// The cut part (or with cut: false, the cutter).
    name: String,
    /// The part to cut; hide it.
    input: String,
    /// The view the line is drawn in: right / left (points [z, y]), front /
    /// back ([x, y]), top / bottom ([x, z]); it lands on the first surface
    /// seen from there (right looks along -x, front along -z, top down).
    view: String,
    /// The line in that view's coordinates, meters.
    points: Vec<[f32; 2]>,
    /// The line closes back to its first point (a door outline).
    #[serde(default)]
    closed: bool,
    /// A smooth curve through the points (Catmull-Rom) instead of straight
    /// segments.
    #[serde(default)]
    smooth: bool,
    /// Groove width, meters (a shut line: 0.004..0.008).
    #[serde(default)]
    width: f32,
    /// Groove depth under the surface; with fill, how far under the
    /// deepest surface point inside the outline the cutter reaches.
    depth: f32,
    /// false: output the cutter itself (hide it; for split or boolean).
    /// Default true: the input with the groove cut.
    #[serde(default)]
    cut: Option<bool>,
    /// A closed outline's whole area as one cutter (a prism along the
    /// view, from above the surface to `depth` under it): a window or lamp
    /// drawn in the view it is seen in. No width.
    #[serde(default)]
    fill: bool,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CageSpec {
    name: String,
    /// Cage vertices [[x, y, z], ...] in model space.
    verts: Vec<[f32; 3]>,
    /// Polygons as vertex indices, mostly quads ([a, b, c, d]), each wound
    /// counter-clockwise seen from outside; triangles and larger polygons
    /// work too. Shared edges run opposite ways in their two faces.
    faces: Vec<Vec<u32>>,
    /// Per-edge crease weights [[a, b, weight], ...]: the edge a-b stays
    /// sharp for `weight` subdivision levels (1.5 = sharp one level, half
    /// way the next), 10 or more = sharp at every level. Default: none.
    #[serde(default)]
    creases: Vec<[f32; 3]>,
    /// "x", "y" or "z": give one half (every vertex on the + side or on
    /// the plane); it is mirrored across that zero plane and joined along
    /// it (vertices on the plane are shared).
    #[serde(default)]
    mirror: Option<String>,
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
    /// Drop the path onto this part's surface (terrain, a hill, a roof):
    /// the path is then [x, y, z] with y a height above the surface
    /// (usually 0), and spacing is measured in plan.
    #[serde(default)]
    on: Option<String>,
    /// With `on`: "upright" (default; copies stand straight, turned only
    /// about y: posts, lamps, trees), "slope" (pitched to follow the ground
    /// along the path: rails, walls, kerbs, steps of a ramp) or "normal"
    /// (standing on the surface's normal: tilted on a slope sideways too).
    #[serde(default)]
    follow: Option<String>,
    /// Copies go between consecutive points instead of at them, each turned
    /// along and stretched over the span from one point to the next (model
    /// the source at any length along +x, centred on the origin):
    /// fence rails between posts, a wall between pillars, rope between
    /// poles. One fewer copy than points; same count/spacing as the posts.
    #[serde(default)]
    between: bool,
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
    /// Omit it with `decal`.
    #[serde(default)]
    input: Option<ScatterInput>,
    /// Scatter decals instead of parts (cracks, stains, fallen leaves):
    /// decals name_0.. projected into `on` along its normal at each point.
    #[serde(default)]
    decal: Option<ScatterDecalSpec>,
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
    /// Only within `near_distance` of these parts' surfaces (clutter along
    /// wall bases, moss round rocks): a part or a list.
    #[serde(default)]
    near: Option<NamesArg>,
    /// Meters from the `near` parts' surfaces (default 0.5).
    #[serde(default)]
    near_distance: Option<f32>,
    /// 0..1: prefer points near `on`'s borders, creases and corners (0 =
    /// uniform, 1 = hug them; falloff a tenth of the surface's span).
    #[serde(default)]
    edge_bias: Option<f32>,
    /// Clusters: {"count": 5, "radius": 1.5} picks 5 centres and gathers
    /// the copies within 1.5 m of them (denser at the centre).
    #[serde(default)]
    clump: Option<crate::scatter::Clump>,
    /// Terrain mask (`on` must be a terrain item): "path" (only on carved
    /// paths), "pad" (only on flattened pads), "not_path", "not_pad".
    #[serde(default)]
    mask: Option<String>,
    /// Only points with world y in [min, max] (meters).
    #[serde(default)]
    height: Option<[f32; 2]>,
}

/// A part name or a list of them.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum NamesArg {
    One(String),
    Many(Vec<String>),
}

/// The decals a decal scatter strews.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScatterDecalSpec {
    /// A procedural material of domain "decal" (or "card").
    pbr: String,
    /// Param overrides for this use of the material.
    #[serde(default)]
    params: Option<std::collections::BTreeMap<String, f64>>,
    /// Image width, meters: a number or a random [min, max] range.
    size: RangeArg,
    /// Image height / width (default 1: square).
    #[serde(default)]
    aspect: Option<f32>,
    /// Box depth along the projection (default half the smaller side).
    #[serde(default)]
    depth: Option<f32>,
    /// "random" (default: each decal turned at random about the normal) or "none".
    #[serde(default)]
    roll: Option<String>,
    /// "clustered" (default) or "forward", as for a decal item.
    #[serde(default)]
    kind: Option<String>,
}

impl ScatterSpec {
    fn into_op(self, ctx: &str) -> Result<Op, String> {
        let input: Vec<Vec<String>> = match self.input {
            None if self.decal.is_some() => Vec::new(),
            None => {
                return Err(format!(
                    "{ctx}: input: name the part(s) to scatter, or give `decal` to scatter decals"
                ));
            }
            Some(_) if self.decal.is_some() => {
                return Err(format!("{ctx}: give input (parts) or decal, not both"));
            }
            Some(ScatterInput::One(s)) => vec![vec![s]],
            Some(ScatterInput::Many(v)) => v
                .into_iter()
                .map(|e| match e {
                    ScatterEntry::One(s) => vec![s],
                    ScatterEntry::Set(v) => v,
                })
                .collect(),
        };
        if (self.decal.is_none() && input.is_empty()) || input.iter().any(Vec::is_empty) {
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
            if self.decal.is_some() {
                return Err(format!(
                    "{ctx}: weights: pick between input entries, not decals"
                ));
            }
            def.weights = w;
        }
        if let Some(n) = self.near {
            let names = match n {
                NamesArg::One(s) => vec![s],
                NamesArg::Many(v) => v,
            };
            if names.is_empty() {
                return Err(format!("{ctx}: near: name at least one part"));
            }
            if names.contains(&self.on) {
                return Err(format!(
                    "{ctx}: near: `{}` is `on` itself (every point is near it; edge_bias hugs its borders)",
                    self.on
                ));
            }
            def.near = names;
        }
        if let Some(d) = self.near_distance {
            if def.near.is_empty() {
                return Err(format!(
                    "{ctx}: near_distance goes with `near` (the parts to stay near)"
                ));
            }
            if !(d.is_finite() && d > 0.0) {
                return Err(format!("{ctx}: near_distance: must be > 0 meters"));
            }
            def.near_distance = Some(d);
        }
        if let Some(b) = self.edge_bias {
            if !(0.0..=1.0).contains(&b) {
                return Err(format!(
                    "{ctx}: edge_bias: 0 (uniform) .. 1 (hug borders and creases)"
                ));
            }
            def.edge_bias = b;
        }
        if let Some(c) = self.clump {
            if c.count == 0 || c.count > 1000 {
                return Err(format!("{ctx}: clump: count 1..1000 centres"));
            }
            if !(c.radius.is_finite() && c.radius > 0.0) {
                return Err(format!("{ctx}: clump: radius must be > 0 meters"));
            }
            def.clump = Some(c);
        }
        if let Some(m) = self.mask {
            if !matches!(m.as_str(), "path" | "pad" | "not_path" | "not_pad") {
                return Err(format!(
                    "{ctx}: mask: `{m}` (one of path, pad, not_path, not_pad)"
                ));
            }
            def.mask = Some(m);
        }
        if let Some([a, b]) = self.height {
            if !(a.is_finite() && b.is_finite()) {
                return Err(format!("{ctx}: height: [min, max] meters"));
            }
            def.height = Some([a.min(b), a.max(b)]);
        }
        if let Some(d) = self.decal {
            let size = match d.size {
                RangeArg::One(x) => [x, x],
                RangeArg::Two(v) => v,
            };
            if size.iter().any(|x| !(x.is_finite() && *x > 0.0)) {
                return Err(format!("{ctx}: decal.size: width must be > 0 meters"));
            }
            let aspect = d.aspect.unwrap_or(1.0);
            if !(aspect.is_finite() && aspect > 0.0) {
                return Err(format!("{ctx}: decal.aspect: height / width must be > 0"));
            }
            if d.depth.is_some_and(|x| !(x.is_finite() && x > 0.0)) {
                return Err(format!("{ctx}: decal.depth: must be > 0 meters"));
            }
            let roll = match d.roll.as_deref() {
                None | Some("random") => true,
                Some("none") => false,
                Some(r) => return Err(format!("{ctx}: decal.roll: `{r}` (random or none)")),
            };
            let kind = d.kind.unwrap_or_else(|| "clustered".into());
            if !matches!(kind.as_str(), "clustered" | "forward") {
                return Err(format!(
                    "{ctx}: decal.kind: `{kind}`: use clustered or forward"
                ));
            }
            if def.align != 0.0
                || def.rotate != "random_y"
                || def.sink != 0.0
                || def.scale != [1.0, 1.0]
            {
                return Err(format!(
                    "{ctx}: align / rotate / sink / scale move parts; a decal scatter sizes decals with decal.size and turns them with decal.roll"
                ));
            }
            return Ok(Op::DecalScatter {
                name: self.name,
                on: self.on,
                def: Box::new(def),
                decal: Box::new(crate::scatter::DecalScatterDef {
                    pbr: crate::material::PbrRef {
                        material: d.pbr,
                        params: d.params.unwrap_or_default(),
                        scale: 1.0,
                        graph: None,
                    },
                    size: [size[0].min(size[1]), size[0].max(size[1])],
                    aspect,
                    depth: d.depth,
                    roll,
                    kind,
                }),
            });
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
    /// Outputs `name_above` and `name_below` (by a plane) or `name_inside`
    /// and `name_outside` (by a cutter).
    name: String,
    /// A closed solid to cut; hide it.
    input: String,
    /// The cutting plane: {"normal": [0, 1, 0], "point": [0, 0.3, 0]}.
    #[serde(default)]
    plane: Option<PlaneSpec>,
    /// Instead of a plane: a closed solid (hide it). The input splits
    /// exactly into the part inside it and the rest, sharing one cut
    /// boundary: glass, lamps, doors, hoods as their own parts.
    #[serde(default)]
    cutter: Option<String>,
    /// "both" (default), "above" / "below" (plane) or "inside" /
    /// "outside" (cutter).
    #[serde(default)]
    keep: Option<String>,
    /// Plane: meters between the halves (each moves half of it along the
    /// normal).
    #[serde(default)]
    gap: f32,
    /// Cutter: only this many meters under the input's surface go inside
    /// (an inlay of skin thickness, not the whole overlap; the inner
    /// boundary is the surface offset inward). Default 0: all of it.
    #[serde(default)]
    depth: f32,
    /// Cutter: move the inside piece this far out along the mean normal of
    /// its outer surface (a lens or pane standing proud, meters).
    #[serde(default)]
    lift: f32,
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
    /// Fillet only: which edges to round, one word or a list: "all"
    /// (default), "top" / "bottom" (horizontal edges in the upper / lower
    /// half of the part's box), "horizontal" (both), "vertical" (edges along
    /// y). Exact for axis-aligned edges; slanted edges follow the tool of
    /// their half.
    #[serde(default)]
    edges: OneOrMany,
}

/// Recessed panels in one face of a closed solid.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PanelSpec {
    name: String,
    /// A closed solid (a wall, hull, door, crate side).
    input: String,
    /// top, bottom, left, right, front or back (world axes; front = +z): the
    /// side of the input's box to panel.
    face: String,
    /// Panels down the face (along y on the sides, z on top/bottom). Default 1.
    #[serde(default)]
    rows: Option<u32>,
    /// Panels across the face. Default 1.
    #[serde(default)]
    cols: Option<u32>,
    /// Margin from the face's edges to the grid, meters (default 0.01). Alias: border.
    #[serde(default, alias = "border")]
    inset: Option<f32>,
    /// How deep each panel is recessed, meters (default 0.005).
    #[serde(default)]
    depth: Option<f32>,
    /// Seam width between panels, meters (default 0.01). Alias: seam.
    #[serde(default, alias = "seam")]
    gap: Option<f32>,
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
    /// over the part's length), "taper" (cross-section from 1 to
    /// `scale_end` along `axis`) or "offset" (every vertex moved `amount`
    /// meters along its normal, + out, - in, even at bends: a pane standing
    /// proud of its body, a skin inside it). Chain deforms for a horn:
    /// taper, then bend.
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
    /// {points: [[x, y, z, radius] or [x, y, z, [rx, ry], roll?], ...],
    /// smooth (default true)}: a tube
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
    points: Option<Vec<crate::sdf::Sect>>,
    #[serde(default)]
    smooth: Option<bool>,
    #[serde(default)]
    part: Option<String>,
    #[serde(default)]
    shapes: Option<Vec<SdfShapeSpec>>,
    /// silhouette: the side outline [[z, y], ...] (points / arcs, as seen
    /// from +x); with `top` [[x, z], ...] and optional `front` [[x, y],
    /// ...] the shape is their intersected extrusions, edges rounded by
    /// `round`, centred by `at` / turned by `rotation`. `smooth: true` runs
    /// a Catmull-Rom through the outline points.
    #[serde(default)]
    side: Option<ProfileArg>,
    #[serde(default)]
    top: Option<ProfileArg>,
    #[serde(default)]
    front: Option<ProfileArg>,
    /// silhouette: {image, view, height | size} (or a list) traced from a
    /// PNG of the project in place of the profile its view names.
    #[serde(default)]
    from_reference: Option<serde_json::Value>,
}

/// Expands silhouette outline rings: points / arcs, an optional closed
/// Catmull-Rom through them, an optional corner rounding.
fn sil_rings(
    arg: Option<ProfileArg>,
    smooth: bool,
    round: f32,
) -> Result<Option<Vec<Vec<[f32; 2]>>>, String> {
    let Some(a) = arg else { return Ok(None) };
    Ok(Some(crate::silhouette::shape_rings(
        a.rings()?,
        smooth,
        round,
    )))
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
        if self.from_reference.is_some() {
            return Err(format!(
                "{at}: from_reference: traced by the server from a project image (apply_ops); not available here"
            ));
        }
        let sil = self.shape == "silhouette";
        let smooth = sil && self.smooth == Some(true);
        let side = sil_rings(self.side, smooth, 0.0).map_err(|e| format!("{at}: side: {e}"))?;
        let top = sil_rings(self.top, smooth, 0.0).map_err(|e| format!("{at}: top: {e}"))?;
        let front = sil_rings(self.front, smooth, 0.0).map_err(|e| format!("{at}: front: {e}"))?;
        Ok(crate::sdf::Shape {
            side,
            top,
            front,
            smooth: if sil { None } else { self.smooth },
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
    /// Local sculpt warps (inflate / push / pinch / flatten / smooth): the
    /// centre, a point [x, y, z] or an anchor: a joint name (skin joints,
    /// body joints: "head"), "part:top" / "part:joint" (sdf), or anchor
    /// words on the field's own box ("top-front").
    #[serde(default)]
    at: Option<crate::deform::WarpAt>,
    /// Local warps: radius of influence in meters (no effect outside).
    #[serde(default)]
    radius: Option<f32>,
    /// Local warps: "smooth" (default), "linear" or "sharp".
    #[serde(default)]
    falloff: Option<String>,
    /// push: the direction the surface moves.
    #[serde(default)]
    direction: Option<[f32; 3]>,
    /// flatten: the plane's normal (the plane passes through `at`).
    #[serde(default)]
    normal: Option<[f32; 3]>,
    /// smooth: the blur kernel's reach in meters (default radius / 3).
    #[serde(default)]
    size: Option<f32>,
    /// Local warps on a mirrored field: true (default) acts on both sides,
    /// false only on the side `at` is on.
    #[serde(default)]
    mirror: Option<bool>,
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
            at: self.at,
            radius: self.radius,
            falloff: self.falloff,
            direction: self.direction,
            normal: self.normal,
            size: self.size,
            mirror: self.mirror,
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
    /// Decimate the result to about this many triangles in the same item
    /// (replaces an sdf + decimate + hide_part triple; the coarser meshing
    /// a decimate needs is picked for you).
    #[serde(default)]
    tris: Option<u32>,
    /// With tris: bake the dense surface into normal + height maps.
    #[serde(default)]
    bake: Option<bool>,
}

/// A skin chain point: [x, y, z, radius] or a joint name.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum ChainPtSpec {
    Joint(String),
    Point(crate::sdf::Sect),
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BodySpec {
    name: String,
    /// biped (a person, 1.75 m), quadruped (a cat, 0.25 m at the
    /// shoulder), bird (0.25 m), fish (0.4 m long), serpent (2 m long),
    /// car (4.3 m long), or human: a parametric template mesh (MakeHuman's
    /// CC0 base mesh + morph targets: gender, age, muscle, weight, shape,
    /// expression; needs the data/human/human.bin file). Facing +z, left
    /// = +x, standing on y = 0.
    preset: String,
    /// Size in meters: standing height (biped, bird), shoulder height
    /// (quadruped), length (fish, serpent, car).
    #[serde(default)]
    height: Option<f32>,
    /// Head size as a fraction of `height` (biped default 1/7.5; others
    /// relative to their default: quadruped 0.36, bird 0.2, fish 0.25,
    /// serpent 0.03, car 0.12 = cabin height).
    #[serde(default)]
    head_ratio: Option<f32>,
    /// Girth: 0.7 slim .. 1 .. 1.5 heavy (car: width).
    #[serde(default)]
    build: Option<f32>,
    /// Leg length factor (car: wheel size), default 1.
    #[serde(default)]
    leg_length: Option<f32>,
    /// Tail length factor, 0 = none (biped default 0, others 1; car: rear
    /// overhang).
    #[serde(default)]
    tail: Option<f32>,
    /// Snout / muzzle / beak / nose length factor (car: front overhang).
    #[serde(default)]
    snout: Option<f32>,
    /// Per-joint edits {"paw_l": {"scale": 1.2, "offset": [0, 0, 0.01]}}:
    /// scale sizes the sections at that joint, offset (meters) moves it and
    /// every joint below it.
    #[serde(default)]
    joints: Option<std::collections::BTreeMap<String, crate::body::JointEdit>>,
    /// Smooth radius where chains meet (default per preset).
    #[serde(default)]
    blend: Option<f32>,
    /// Meshing voxel size in meters (default from the size).
    #[serde(default)]
    resolution: Option<f32>,
    /// Decimate to about this many triangles in the same item.
    #[serde(default)]
    tris: Option<u32>,
    /// With tris: bake the dense surface into normal + height maps.
    #[serde(default)]
    bake: Option<bool>,
    /// false: no automatic `<name>_rig` skeleton (default true). human:
    /// "game" (default, ~58 bones named like the biped's) or "full" (the
    /// template's whole skeleton with face bones, `xxx.L` -> `xxx_l`).
    #[serde(default)]
    rig: Option<RigArg>,
    /// false: no automatic smooth `<name>_bind` of the body to its rig
    /// (default true with the rig; a `bind` item on the rig replaces it).
    #[serde(default)]
    bind: Option<bool>,
    /// Field warps, as on sdf; local sculpt warps may be centred on a joint
    /// by name ({"mode": "inflate", "at": "chest", "radius": 0.15,
    /// "amount": 0.02}).
    #[serde(default)]
    warps: Option<Vec<WarpSpec>>,
    /// human: 0 female .. 1 male (default 0.5).
    #[serde(default)]
    gender: Option<f32>,
    /// human: age in years (> 1; default 25), or MakeHuman's 0..1 scale.
    #[serde(default)]
    age: Option<f32>,
    /// human: 0..1 (default 0.5).
    #[serde(default)]
    muscle: Option<f32>,
    /// human: body fat 0..1 (default 0.5).
    #[serde(default)]
    weight: Option<f32>,
    /// human: 0..1, 0.5 neutral (above: idealized, below: uncommon).
    #[serde(default)]
    proportions: Option<f32>,
    /// human: 0..1 (default 0.5; female share only).
    #[serde(default)]
    breast_size: Option<f32>,
    /// human: 0..1 (default 0.5).
    #[serde(default)]
    breast_firmness: Option<f32>,
    /// human: {"african", "asian", "caucasian"} weights (normalized;
    /// default equal).
    #[serde(default)]
    ethnicity: Option<std::collections::BTreeMap<String, f32>>,
    /// human: detail targets by name, -1..1 between their decrease and
    /// increase files ({"nose-width1": 0.3, "head-fat": -0.5}).
    #[serde(default)]
    shape: Option<std::collections::BTreeMap<String, f32>>,
    /// human: expression units 0..1 ({"mouth-open": 0.6,
    /// "mouth-corner-puller": 1}).
    #[serde(default)]
    expression: Option<std::collections::BTreeMap<String, f32>>,
    /// human: Catmull-Clark levels over the quad cage, 0..2 (default 0:
    /// ~27k triangles; 1: ~110k).
    #[serde(default)]
    detail: Option<u32>,
    /// human: extras to make, separate parts `<name>_eyes`, `<name>_teeth`,
    /// `<name>_tongue` (default all three; [] for none).
    #[serde(default)]
    parts: Option<Vec<String>>,
    /// human: named morph targets {"smile": {"expression": {...},
    /// "shape": {...}}}: deltas against this item, exported as glTF morph
    /// targets and keyed by a clip's `morphs`.
    #[serde(default)]
    shapes: Option<std::collections::BTreeMap<String, crate::human::ShapeKey>>,
    /// human: iris colour of the built-in eye material ("#5a3a22",
    /// [r, g, b]; default a mid brown).
    #[serde(default)]
    eye_color: Option<Value>,
    /// human: pupil size, 1 = normal (0.5..2).
    #[serde(default)]
    pupil: Option<f32>,
    /// human: body measurements to hit, meters ({"chest": 1.0, "waist":
    /// 0.85, "hips": 1.0, "neck": 0.38, "upper_arm": 0.31, "thigh": 0.58,
    /// "calf": 0.37}): girths solved through MakeHuman's measure targets.
    #[serde(default)]
    measure: Option<std::collections::BTreeMap<String, f32>>,
    /// human: skin tone, 0 (light) .. 1 (dark) along a melanin ramp, or a
    /// colour; lips, nails, areolae and flushed areas follow it.
    #[serde(default)]
    skin_tone: Option<crate::human::look::SkinTone>,
    /// human: hair, a style (bald, buzz, short (default), crop, bob, long,
    /// ponytail, bun, afro, curly) or {style, length, color, part (left /
    /// right / middle / none), fringe 0..1, volume 0..1, cards, tris, seed}:
    /// part `<name>_hair` (a feathered scalp cap plus strand cards).
    #[serde(default)]
    hair: Option<Box<crate::human::hair::HairIn>>,
    /// human: eyebrows (`<name>_brows`), false or {thickness, arch -1..1, color}.
    #[serde(default)]
    brows: Option<Box<crate::human::hair::Toggle<crate::human::hair::BrowsDef>>>,
    /// human: eyelashes (`<name>_lashes`), false or {length, curl 0..1, color}.
    #[serde(default)]
    lashes: Option<Box<crate::human::hair::Toggle<crate::human::hair::LashesDef>>>,
}

/// `rig`: true / false, or a human's rig kind ("game" / "full").
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum RigArg {
    On(bool),
    Kind(String),
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SilhouetteSpec {
    name: String,
    /// Side outline seen from +x (the right): [[z, y], ...] points or arcs
    /// (front = +z). Or give it with `from_reference`.
    #[serde(default)]
    side: Option<ProfileArg>,
    /// Plan outline: [[x, z], ...].
    #[serde(default)]
    top: Option<ProfileArg>,
    /// Optional front outline seen from +z: [[x, y], ...].
    #[serde(default)]
    front: Option<ProfileArg>,
    /// A closed centripetal Catmull-Rom through each outline's points (a few
    /// points make a smooth body), default false.
    #[serde(default)]
    smooth: Option<bool>,
    /// Round every outline corner by this radius (meters, in 2D).
    #[serde(default)]
    round: f32,
    /// Smooth radius of the intersection, meters: rounds the solid's edges
    /// where the outlines meet (default 0 = sharp).
    #[serde(default)]
    blend: Option<f32>,
    /// {image, view, height | size} or a list: trace the subject of a PNG in
    /// the project (as the agent_views reference compare finds it) into the
    /// profile its view names (front / back -> front, side / right / left
    /// -> side, top -> top). `height` scales the outline's vertical extent
    /// to meters (uniform); `size` [w, h] sets both. Front / side outlines
    /// stand on y = 0, centred; a side image is seen from the right (image
    /// right = -z), `left` from the left.
    #[serde(default)]
    from_reference: Option<serde_json::Value>,
    /// Filled by the server from `from_reference` (image, view, content
    /// hash): the op re-traces when the image changes. Not written by hand.
    #[serde(default)]
    references: Vec<crate::silhouette::RefSource>,
    /// Meshing voxel size in meters (default from the size).
    #[serde(default)]
    resolution: Option<f32>,
    /// Decimate to about this many triangles in the same item.
    #[serde(default)]
    tris: Option<u32>,
    /// With tris: bake the dense surface into normal + height maps.
    #[serde(default)]
    bake: Option<bool>,
    #[serde(default, alias = "position")]
    translation: [f32; 3],
    #[serde(default)]
    rotation: Option<RotationSpec>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SkinSpec {
    name: String,
    /// Chains of points, each [x, y, z, radius], an elliptical section
    /// [x, y, z, [rx, ry], roll?] (rx sideways, ry along the tube's up,
    /// roll degrees about the tangent) or a joint name: a tube
    /// through them whose radius follows the points. Chains sharing a joint
    /// melt together there (a limb into a body, fingers into a palm).
    chains: Vec<Vec<ChainPtSpec>>,
    /// Named points [x, y, z, radius] (or [x, y, z, [rx, ry], roll?])
    /// chains can share.
    #[serde(default)]
    joints: Option<std::collections::BTreeMap<String, crate::sdf::Sect>>,
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
    /// Decimate the result to about this many triangles in the same item
    /// (replaces an sdf + decimate + hide_part triple; the coarser meshing
    /// a decimate needs is picked for you).
    #[serde(default)]
    tris: Option<u32>,
    /// With tris: bake the dense surface into normal + height maps.
    #[serde(default)]
    bake: Option<bool>,
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
    /// subdiv: a triangle budget instead of `levels`: subdivides to the
    /// first level at or past it, then decimates to it (one op instead of
    /// subdiv + decimate).
    #[serde(default)]
    tris: Option<u32>,
    /// subdiv with tris: "x" / "y" / "z" decimates symmetrically about
    /// that zero plane (as decimate's `symmetric`).
    #[serde(default)]
    symmetric: Option<String>,
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
    /// "x", "y" or "z": keep mirror symmetry exactly: the + half is
    /// decimated and mirrored across that zero plane (the part must be
    /// centred on it; its - half is replaced by the mirror image).
    #[serde(default)]
    symmetric: Option<String>,
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
    /// "exact": a ball erosion (corners stay sharp; inputs up to 20,000
    /// triangles). "offset": every vertex moved in along its normal, even
    /// at bends, for dense smooth meshes (a subdivided body); no `open`.
    /// Default: exact up to 20,000 triangles, offset above.
    #[serde(default)]
    method: Option<String>,
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
    /// Left/right: [z, y]. Or an anchor whose cross-axis coordinates are
    /// taken: a body joint ("cat:paw_l") or a box anchor ("desk:top-front").
    #[serde(default)]
    at: Option<crate::PlaceAt>,
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
    /// Rounded-edge radius (meters) baked into the normal map at sharp
    /// creases (no geometry change); gives the part its own atlas bake.
    #[serde(default)]
    bevel: Option<f32>,
    /// Low-frequency world-space colour / roughness drift and darkening
    /// toward y = 0: {amount 0.15, scale 4, ground_dirt 0, ground_height
    /// 0.5, seed 0}. Gives the part its own atlas bake.
    #[serde(default, rename = "macro")]
    macro_: Option<crate::material::finish::Macro>,
}

/// One look of a `variant`: the fields of a `material` item, minus name
/// and input (the map key names the parts).
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VariantLookSpec {
    #[serde(default, alias = "color")]
    base_color: Option<ColorArg>,
    #[serde(default)]
    roughness: Option<f32>,
    #[serde(default)]
    metalness: Option<f32>,
    /// A procedural material (material_ops / material_catalog name).
    #[serde(default)]
    pbr: Option<String>,
    #[serde(default)]
    params: Option<std::collections::BTreeMap<String, f64>>,
    #[serde(default)]
    scale: Option<f32>,
    /// A plain colour on purpose (drops the base look's pbr).
    #[serde(default)]
    flat: bool,
    #[serde(default)]
    bevel: Option<f32>,
    #[serde(default, rename = "macro")]
    macro_: Option<crate::material::finish::Macro>,
}

/// A cosmetic variant ("skin"): alternate materials for some parts and/or a
/// style token patch; geometry never changes. The base look is "default".
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VariantSpec {
    /// The variant's name ("red_team"; not "default").
    name: String,
    /// Part name or `*` glob -> the look those parts wear in this variant
    /// (material item fields). Globs apply first, then exact names.
    #[serde(default)]
    materials: std::collections::BTreeMap<String, VariantLookSpec>,
    /// Style token patch for this variant ({"colors": {"primary": "#c33"}}):
    /// items reading tokens re-resolve, like a prefab's style patch.
    #[serde(default)]
    #[schemars(with = "Option<serde_json::Value>")]
    style: Option<serde_json::Value>,
    /// Parts (names / `*` globs) shown in this variant: an outfit's
    /// garments, hidden in the base model.
    #[serde(default)]
    show: OneOrMany,
    /// Parts hidden in this variant (another outfit's pieces).
    #[serde(default)]
    hide: OneOrMany,
}

/// A garment item: `name` plus [`crate::garment::GarmentDef`].
#[derive(schemars::JsonSchema)]
pub struct GarmentSpec {
    /// The garment part's name.
    pub name: String,
    #[serde(flatten)]
    pub def: crate::garment::GarmentDef,
}

/// A stairs item: `name` plus [`crate::stairs::StairsDef`].
#[derive(schemars::JsonSchema)]
pub struct StairsSpec {
    /// The flight's part name (`name_blk_<i>` for its collider markers).
    pub name: String,
    #[serde(flatten)]
    pub def: crate::stairs::StairsDef,
}

/// A railing item: `name` plus [`crate::stairs::RailingDef`].
#[derive(schemars::JsonSchema)]
pub struct RailingSpec {
    /// The railing's part name (`name_blk_<i>` for its collider markers).
    pub name: String,
    #[serde(flatten)]
    pub def: crate::stairs::RailingDef,
}

/// A carve item: `name` plus [`crate::carve::CarveDef`].
#[derive(schemars::JsonSchema)]
pub struct CarveSpec {
    /// Base name of the sheets (`name`, or `name_floor`, `name_<cutter>`...
    /// by `split_by`; `name_outer` for the outside skin).
    pub name: String,
    #[serde(flatten)]
    pub def: crate::carve::CarveDef,
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
    /// box, planar (along the thinnest axis), cylindrical, spherical, metric (box at real scale: uv in meters) or face / label (one image per face, upright and unmirrored from outside).
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
    /// {"Place": {"name": "wendys"}}. Written by a .bsn / .tscn scene export.
    #[serde(default)]
    #[schemars(with = "Option<serde_json::Value>")]
    components: serde_json::Map<String, Value>,
    /// One of the prefab's cosmetic variants ("red_team"); omitted: its
    /// default look. A .bsn export writes it for game code.
    #[serde(default)]
    variant: Option<String>,
    /// Also make a `block` marker `<name>_blk` (a `Block` collider) filling
    /// the instance's world bounds: for props that should be solid in games
    /// that build colliders from markers. Default false.
    #[serde(default)]
    block: bool,
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
    /// {"Place": {"name": "wendys"}}. Written by a .bsn / .tscn scene export.
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
    /// Rest rotation of the bone's frame in world space (any rotation
    /// form). Default: world-aligned. With a rest rotation, clip rotations
    /// of this bone turn about its own local axes.
    #[serde(default)]
    rest: Option<RotationSpec>,
    /// Degrees about the head -> tail axis, with the frame's +y along the
    /// bone (applied after `rest` when both are given). Setting it (even 0)
    /// aligns the bone's frame to the bone.
    #[serde(default)]
    roll: Option<f32>,
    /// Joint limits, degrees per axis of the bone's rest frame: `{"x":
    /// [-10, 140]}`. The clip rotation (offset from rest) is split into
    /// Euler angles in the order rotations `{x, y, z}` apply (x, then y,
    /// then z) and each named axis is clamped, after the tracks and after
    /// IK. Mirror twins get y and z flipped.
    #[serde(default)]
    limits: Option<crate::anim::Limits>,
    /// Aim the bone after IK: `{"target": [x, y, z] | "part:anchor" |
    /// "bone", "axis"?: "z" | "-x" | [x, y, z] (in the rest frame, default
    /// head -> tail), "weight"?: 0..1}` (eyes, a head tracking a target).
    #[serde(default)]
    look_at: Option<LookAtSpec>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LookAtSpec {
    /// A point, a part anchor ("ball:center") or a bone of the skeleton.
    target: crate::anim::PointRef,
    /// "x" / "-z" / [x, y, z] in the bone's rest frame (default head -> tail).
    #[serde(default)]
    #[schemars(with = "Option<serde_json::Value>")]
    axis: Option<Value>,
    /// 0..1 (default 1).
    #[serde(default)]
    weight: Option<f32>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SkeletonSpec {
    name: String,
    /// Bones by name. Rest frames are world-aligned unless a bone sets
    /// `rest` / `roll`: a clip's rotation `{"x": 25}` pitches the bone
    /// about its rest frame's x (world x by default).
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
    /// Smooth: weights from the closest point of this bound part's surface
    /// (clothing, armour, hair caps over a body); `blend` then smooths them.
    #[serde(default)]
    weights_from: Option<String>,
    /// Smooth: pose-driven corrective shapes: true / false, or {joints,
    /// poses: [{bone, axis, degrees}], max} (default on for the human).
    #[serde(default)]
    correctives: Option<crate::anim::skin::CorrectivesDef>,
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

/// A clip from motion capture: a BVH file retargeted onto a skeleton.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MocapSpec {
    name: String,
    skeleton: String,
    /// A BVH file in the project (resolved like `ops_file`; a changed file
    /// re-derives the clip).
    #[serde(default)]
    file: Option<String>,
    /// Inline BVH text instead of `file`.
    #[serde(default)]
    bvh: Option<String>,
    /// Set by the server (the file content's hash); not written by hand.
    #[serde(default)]
    hash: Option<String>,
    /// "auto" (default: by name, CMU / Mixamo / 3ds Max biped conventions),
    /// or {our bone: BVH joint} overrides on top of it ("" unmaps a bone);
    /// {"only": {...}} uses just those entries.
    #[serde(default)]
    map: Option<Value>,
    /// [t0, t1] seconds of the file.
    #[serde(default)]
    range: Option<[f32; 2]>,
    /// Keys per second (default 30).
    #[serde(default)]
    fps: Option<f32>,
    /// false (default), true, "blend" (crossfade the end into the start for
    /// a seamless loop, over `blend` seconds, default 0.25) or {"blend": s}.
    #[serde(default, rename = "loop")]
    looping: Option<Value>,
    #[serde(default)]
    blend: Option<f32>,
    /// Root translation scale (default: target / source leg length, so
    /// file units cancel).
    #[serde(default)]
    scale: Option<f32>,
    /// "in_place" (default: travel removed, its speed reported and kept on
    /// the clip), true (root motion on xz) or {"axes": "xz"}.
    #[serde(default)]
    root_motion: Option<Value>,
    /// Foot planting: true (default), false or {plant, contact_height 0.03,
    /// contact_speed 0.15}.
    #[serde(default)]
    feet: Option<Value>,
    /// Hand planting, the same fields (default off).
    #[serde(default)]
    hands: Option<Value>,
    /// Swap left and right.
    #[serde(default)]
    mirror: bool,
    /// Playback rate (2 = twice as fast).
    #[serde(default)]
    speed: Option<f32>,
    /// Degrees about +y, or "auto" (turn so the travel goes toward +z).
    #[serde(default)]
    facing: Option<Value>,
    /// linear (default) or smooth.
    #[serde(default)]
    interp: Option<String>,
}

fn plant_of(
    ctx: &str,
    field: &str,
    v: Option<&Value>,
    default: bool,
) -> Result<crate::mocap::Plant, String> {
    let mut p = if default {
        crate::mocap::Plant::on()
    } else {
        crate::mocap::Plant::off()
    };
    match v {
        None | Some(Value::Null) => {}
        Some(Value::Bool(b)) => p.plant = *b,
        Some(Value::Object(o)) => {
            p.plant = true;
            for (k, x) in o {
                match (k.as_str(), x) {
                    ("plant", Value::Bool(b)) => p.plant = *b,
                    ("contact_height", x) if x.is_number() => {
                        p.contact_height = x.as_f64().unwrap_or(0.03) as f32
                    }
                    ("contact_speed", x) if x.is_number() => {
                        p.contact_speed = x.as_f64().unwrap_or(0.15) as f32
                    }
                    _ => {
                        return Err(format!(
                            "{ctx}: {field}.{k}: fields are plant (bool), contact_height (m), contact_speed (m/s)"
                        ));
                    }
                }
            }
        }
        Some(other) => {
            return Err(format!(
                "{ctx}: {field}: true, false or {{plant, contact_height, contact_speed}}, got {}",
                describe_json(other)
            ));
        }
    }
    Ok(p)
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClipSpec {
    name: String,
    skeleton: String,
    /// Seconds (with `over`, default the base clip's; a whole number of
    /// its cycles when both loop).
    #[serde(default)]
    duration: Option<f32>,
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
    /// Inverse kinematics, keyed by the END bone whose head (its joint) is
    /// driven to a target: `{"ankle_l": {"target": [x, y, z] |
    /// "part:anchor" | [[t, [x, y, z]], ...], "chain": 2, "pole"?: [x, y,
    /// z] | anchor, "weight"?: 0..1 or keys, "rotate"?: keys}}`. Solved
    /// after the tracks; `chain` bones above the end bend (2: analytic,
    /// the middle joint toward `pole`, else the bend the pose already
    /// has; more: FABRIK). `weight` blends FK (0) to IK (1); `rotate` keys
    /// the end bone's world rotation as an offset from rest (a foot kept
    /// flat). `mirror` copies an entry too (target x negated).
    #[schemars(with = "std::collections::BTreeMap<String, serde_json::Value>")]
    #[serde(default)]
    ik: std::collections::BTreeMap<String, Value>,
    /// Whole-pose keys `[[t, "pose"], ...]` (`pose` items; `"rest"` is
    /// built in): each key keys every bone either neighbouring pose
    /// touches, with the clip's `interp`. An explicit track channel on a
    /// bone overrides what the poses key on that channel.
    #[schemars(with = "Vec<serde_json::Value>")]
    #[serde(default)]
    poses: Vec<Value>,
    /// A keyed clip of the same skeleton this one layers over: its tracks
    /// replace the base's on their bones (blended by `weight`), or with
    /// `additive` compose on top (rotation after the base's, translation
    /// added, scale multiplied). Limits, IK (base entries first) and
    /// look_at then run once on the result.
    #[serde(default)]
    over: Option<String>,
    #[serde(default)]
    additive: bool,
    /// With `over`: 0..1 (a number or keys `[[t, w], ...]`; default 1).
    #[schemars(with = "Option<serde_json::Value>")]
    #[serde(default)]
    weight: Option<Value>,
    /// With `over`: the bones the layer touches (`*` globs).
    #[serde(default)]
    bones: Vec<String>,
    /// `true` (bone `hip`, else the first root bone; axes `xz`) or
    /// `{"bone", "axes": "xz", "yaw": false}`: that root bone's travel is
    /// the character's motion. The modeler plays it travelling; exports
    /// remove it from the bone and write it to the animation's
    /// `extras.root_motion` for the game to move the entity.
    #[schemars(with = "Option<serde_json::Value>")]
    #[serde(default)]
    root_motion: Option<Value>,
    /// `[[t, "name"], [t, "name", data], ...]`: named moments (footsteps),
    /// exported in the animation's `extras.events`.
    #[schemars(with = "Vec<serde_json::Value>")]
    #[serde(default)]
    events: Vec<Value>,
    /// Morph target weights per part (a `human` body's name) and shape:
    /// `{"eve": {"smile": [[0, 0], [0.5, 1], [1, 0]], "jaw_open": 0.3,
    /// "blink": {"wave": {"amount": 0.5, "period": 2}}}}`: keys `[[t, w],
    /// ...]` (the clip's interp, or `{"keys", "interp"}`), a constant, or
    /// a wave. Weights are usually 0..1 but not clamped. A layer (`over`)
    /// replaces its base's weight (by `weight`) or, `additive`, adds.
    #[schemars(
        with = "std::collections::BTreeMap<String, std::collections::BTreeMap<String, serde_json::Value>>"
    )]
    #[serde(default)]
    morphs: std::collections::BTreeMap<String, std::collections::BTreeMap<String, Value>>,
}

/// One morph weight channel of a clip: keys `[[t, w], ...]`, `{"keys",
/// "interp"}`, a number (constant) or `{"wave": {"amount", "period"?,
/// "phase"?}}` (the weight in x).
fn morph_channel(
    ctx: &str,
    v: &Value,
    duration: f32,
    default: crate::anim::Interp,
) -> Result<crate::anim::Channel, String> {
    use crate::anim::Channel;
    match v {
        Value::Number(n) => {
            let w = n.as_f64().unwrap_or(0.0) as f32;
            Ok(Channel::Keys {
                keys: vec![(0.0, [w, w, w, 0.0])],
                interp: crate::anim::Interp::Linear,
            })
        }
        Value::Object(o) if o.contains_key("wave") => {
            let mut o = o.clone();
            if let Some(Value::Object(w)) = o.get_mut("wave") {
                if w.contains_key("axis") {
                    return Err(format!("{ctx}: wave: a morph weight has no axis"));
                }
                w.insert("axis".into(), Value::String("x".into()));
            }
            channel(ctx, "scale", &Value::Object(o), duration, default)
        }
        Value::Array(keys) => {
            for (k, item) in keys.iter().enumerate() {
                let ok = item
                    .as_array()
                    .is_some_and(|a| a.len() == 2 && a[0].is_number() && a[1].is_number());
                if !ok {
                    return Err(format!(
                        "{ctx}: key #{k}: expected [t, weight] (numbers), got {}",
                        describe_json(item)
                    ));
                }
            }
            channel(ctx, "scale", v, duration, default)
        }
        Value::Object(_) => channel(ctx, "scale", v, duration, default),
        other => Err(format!(
            "{ctx}: expected keys [[t, w], ...], a number or {{\"wave\": ...}}, got {}",
            describe_json(other)
        )),
    }
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PoseBoneSpec {
    #[serde(default)]
    rotate: Option<RotationSpec>,
    #[serde(default)]
    translate: Option<[f32; 3]>,
    /// A factor or [x, y, z].
    #[schemars(with = "Option<serde_json::Value>")]
    #[serde(default)]
    scale: Option<Value>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PoseSpec {
    name: String,
    skeleton: String,
    /// Per bone: `{"rotate"?: rotation, "translate"?: [x, y, z],
    /// "scale"?: s}`, offsets from rest like clip track values.
    #[serde(default)]
    bones: std::collections::BTreeMap<String, PoseBoneSpec>,
    /// Fill each `_r` bone from its `_l` twin mirrored (and back) where
    /// it isn't given; IK entries too.
    #[serde(default)]
    mirror: bool,
    /// IK solved once when the pose is built: `{"ankle_l": {"target":
    /// [x, y, z] | "part:anchor", "chain"?, "pole"?, "rotate"?}}`.
    #[schemars(with = "std::collections::BTreeMap<String, serde_json::Value>")]
    #[serde(default)]
    ik: std::collections::BTreeMap<String, Value>,
    /// Morph target weights per part and shape (`{"eve": {"smile": 1}}`),
    /// keyed with the pose by a clip's `poses` (0 where a pose leaves a
    /// shape alone).
    #[serde(default)]
    morphs: std::collections::BTreeMap<String, std::collections::BTreeMap<String, f32>>,
}

/// An axis word ("x", "-z") or a vector.
fn signed_axis(ctx: &str, v: &Value) -> Result<[f32; 3], String> {
    if let Value::String(s) = v {
        let (neg, w) = match s.strip_prefix('-') {
            Some(w) => (-1.0, w),
            None => (1.0, s.strip_prefix('+').unwrap_or(s)),
        };
        let a = axis_of(ctx, &Value::String(w.to_string()))?;
        return Ok([a[0] * neg, a[1] * neg, a[2] * neg]);
    }
    axis_of(ctx, v)
}

/// One `ik` entry of a clip.
fn ik_entry(
    ctx: &str,
    v: &Value,
    duration: f32,
    default: crate::anim::Interp,
) -> Result<crate::anim::IkDef, String> {
    use crate::anim::{Channel, IkTarget, Interp};
    let Value::Object(o) = v else {
        return Err(format!(
            "{ctx}: expected {{target, chain?, pole?, weight?, rotate?}}"
        ));
    };
    for k in o.keys() {
        if !matches!(
            k.as_str(),
            "target" | "chain" | "pole" | "weight" | "rotate"
        ) {
            return Err(format!(
                "{ctx}: unknown field `{k}` (target, chain, pole, weight, rotate)"
            ));
        }
    }
    let Some(t) = o.get("target") else {
        return Err(format!(
            "{ctx}: needs target: [x, y, z], an anchor \"part:words\" or keys [[t, [x, y, z]], ...]"
        ));
    };
    let tctx = format!("{ctx}.target");
    let target = match t {
        Value::String(a) if a.contains(':') => IkTarget::Anchor(a.clone()),
        Value::String(a) => {
            return Err(format!(
                "{tctx} `{a}`: an anchor names a part and words, like \"crate:top\""
            ));
        }
        Value::Array(a) if a.first().is_some_and(Value::is_number) => {
            IkTarget::Point(de(&tctx, t)?)
        }
        _ => match channel(&tctx, "translate", t, duration, default)? {
            Channel::Wave { .. } => {
                return Err(format!("{tctx}: a point, an anchor or keys, not a wave"));
            }
            c => IkTarget::Keys(c),
        },
    };
    let chain = match o.get("chain") {
        None => 2,
        Some(c) => match c.as_u64() {
            Some(n @ 1..=32) => n as u32,
            _ => return Err(format!("{ctx}.chain: a whole number of bones, 1..32")),
        },
    };
    let pole: Option<crate::anim::PointRef> = o
        .get("pole")
        .map(|p| de(&format!("{ctx}.pole"), p))
        .transpose()?;
    if let Some(crate::anim::PointRef::Anchor(a)) = &pole
        && !a.contains(':')
    {
        return Err(format!(
            "{ctx}.pole `{a}`: an anchor names a part and words, like \"leg_l:front\""
        ));
    }
    let wctx = format!("{ctx}.weight");
    let weight = match o.get("weight") {
        None => None,
        Some(Value::Number(n)) => {
            let w = n.as_f64().unwrap_or(1.0) as f32;
            if !(0.0..=1.0).contains(&w) {
                return Err(format!("{wctx} must be 0..1"));
            }
            Some(Channel::Keys {
                keys: vec![(0.0, [w, w, w, 0.0])],
                interp: Interp::Linear,
            })
        }
        Some(w) => {
            let c = channel(&wctx, "scale", w, duration, default)?;
            if let Channel::Keys { keys, .. } = &c
                && keys.iter().any(|(_, v)| !(0.0..=1.0).contains(&v[0]))
            {
                return Err(format!("{wctx}: key values must be 0..1"));
            }
            Some(c)
        }
    };
    let rotate = o
        .get("rotate")
        .map(|r| channel(&format!("{ctx}.rotate"), "rotate", r, duration, default))
        .transpose()?;
    Ok(crate::anim::IkDef {
        target,
        chain,
        pole,
        weight,
        rotate,
    })
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
            if !period.is_finite() {
                return Err(format!(
                    "{ctx}: wave needs a period when the duration comes from `over`"
                ));
            }
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
        Value::Object(o) if o.contains_key("euler") => {
            if kind != "rotate" {
                return Err(format!("{ctx}: euler keys are for rotate"));
            }
            for k in o.keys() {
                if !matches!(k.as_str(), "euler" | "interp") {
                    return Err(format!("{ctx}: unknown field `{k}` (euler, interp)"));
                }
            }
            let interp = match o.get("interp").and_then(Value::as_str) {
                Some(s) => interp_of(ctx, s)?,
                None => default,
            };
            let Some(Value::Array(items)) = o.get("euler") else {
                return Err(format!(
                    "{ctx}: euler: expected [[t, {{x, y, z}} degrees], ...]"
                ));
            };
            let mut keys: Vec<(f32, [f32; 4])> = Vec::new();
            for (k, item) in items.iter().enumerate() {
                let kctx = format!("{ctx}: euler key #{k}");
                let Some([t, val]) = item
                    .as_array()
                    .map(Vec::as_slice)
                    .and_then(|a| <&[Value; 2]>::try_from(a).ok())
                else {
                    return Err(format!("{kctx}: expected [t, {{x, y, z}}]"));
                };
                let t = t.as_f64().ok_or(format!("{kctx}: time must be a number"))? as f32;
                if t.is_nan() || t < -1e-6 || t > duration + 1e-4 {
                    return Err(format!("{kctx}: time {t} is outside 0..{duration}"));
                }
                let e: [f32; 3] = if val.is_array() {
                    de(&kctx, val)?
                } else {
                    let e: Euler = de(&kctx, val)?;
                    [e.x, e.y, e.z]
                };
                if e.iter().any(|v| !v.is_finite()) {
                    return Err(format!("{kctx}: degrees must be finite"));
                }
                keys.push((t, [e[0], e[1], e[2], 0.0]));
            }
            if keys.is_empty() {
                return Err(format!("{ctx}: euler needs at least one key"));
            }
            keys.sort_by(|a, b| a.0.total_cmp(&b.0));
            if keys.windows(2).any(|w| (w[1].0 - w[0].0).abs() < 1e-6) {
                return Err(format!("{ctx}: two keys at the same time"));
            }
            return Ok(Channel::Euler { keys, interp });
        }
        Value::Object(o) => {
            for k in o.keys() {
                if !matches!(k.as_str(), "keys" | "interp") {
                    return Err(format!(
                        "{ctx}: unknown field `{k}` (keys, interp, or wave / euler)"
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
    let (keys_v, interp) = match keys_v {
        // `{"keys": {"interp": "smooth", "0": v, "0.5": v}}`: a time -> value
        // map, the form agents reach for, possibly with the interp inside.
        Value::Object(m) => {
            let interp = match m.get("interp").and_then(Value::as_str) {
                Some(s) => interp_of(ctx, s)?,
                None => interp,
            };
            let mut items = Vec::new();
            for (k, v) in &m {
                if k == "interp" {
                    continue;
                }
                let Ok(t) = k.trim().parse::<f64>() else {
                    return Err(format!(
                        "{ctx}: keys: `{k}` is not a time: give keys as [[t, value], ...] (or {{\"0\": value, \"0.5\": value}})"
                    ));
                };
                items.push(serde_json::json!([t, v]));
            }
            (Value::Array(items), interp)
        }
        v => (v, interp),
    };
    let Value::Array(items) = keys_v else {
        return Err(format!(
            "{ctx}: keys must be an array of [t, value] (or {{\"0\": value, \"0.5\": value}}), got {}",
            describe_json(&keys_v)
        ));
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
                let a: [f32; 3] = de(&kctx, val).map_err(|e| {
                    format!(
                        "{e} ({kind} values are [x, y, z]{})",
                        if kind == "scale" {
                            " or one number"
                        } else {
                            " meters"
                        }
                    )
                })?;
                [a[0], a[1], a[2], 0.0]
            }
        };
        keys.push((t, v));
    }
    if keys.is_empty() {
        return Err(format!("{ctx}: needs at least one key"));
    }
    keys.sort_by(|a, b| a.0.total_cmp(&b.0));
    if let Some(w) = keys.windows(2).find(|w| (w[1].0 - w[0].0).abs() < 1e-6) {
        return Err(format!("{ctx}: two keys at the same time ({})", w[0].0));
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
    Body(BodySpec),
    Silhouette(SilhouetteSpec),
    Smooth(SmoothSpec),
    Decimate(DecimateSpec),
    Shell(ShellSpec),
    Transform(TransformSpec),
    Move(MoveSpec),
    Light(LightSpec),
    Decal(DecalSpec),
    Collider(ColliderSpec),
    Marker(MarkerSpec),
    Variant(VariantSpec),
    Skeleton(SkeletonSpec),
    Bind(BindSpec),
    Clip(ClipSpec),
    Mocap(MocapSpec),
    Pose(PoseSpec),
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
    "variant",
    "skeleton",
    "bind",
    "clip",
    "mocap",
    "pose",
    "sway",
    "primitive",
    "extrude",
    "revolve",
    "group",
    "sweep",
    "loft",
    "cage",
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
    "body",
    "silhouette",
    "smooth",
    "decimate",
    "shell",
    "garment",
    "carve",
    "stairs",
    "railing",
    "panel",
    "panel_line",
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
    expand_repeats_noted(items, params).map(|(out, _)| out)
}

/// [`expand_repeats`] plus a note per repeat that made nothing (empty
/// `items`, `count` 0 or empty `values`): those are no-ops, not errors, so
/// generated templates with nothing to repeat still apply.
pub fn expand_repeats_noted(
    items: &[Value],
    params: &crate::params::Params,
) -> Result<(Vec<(Value, Option<String>)>, Vec<String>), String> {
    let mut out = Vec::new();
    let mut notes = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let ctx = format!("op #{i} (repeat)");
        expand_one(item, &ctx, None, params, &mut out, &mut notes)?;
    }
    Ok((out, notes))
}

fn expand_one(
    item: &Value,
    ctx: &str,
    note: Option<&str>,
    params: &crate::params::Params,
    out: &mut Vec<(Value, Option<String>)>,
    notes: &mut Vec<String>,
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
    if r.items.is_empty() || values.is_empty() {
        let why = if r.items.is_empty() {
            "items is empty"
        } else {
            "no values (count 0)"
        };
        notes.push(format!("note: {ctx}: {why}, so it made nothing"));
        return Ok(());
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
                notes,
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

const PRIMITIVE_ROUND_HINT: &str = "primitive round is a number (meters), e.g. round: 0.03, \
or {\"radius\": 0.03, \"segments\"?: 4}; for edge selection use a `round` item with mode fillet";

/// A primitive's `round` given as an object (`{"radius": r, "segments"?: n}`,
/// the shape agents borrow from the `round` op) becomes the plain `round` +
/// `segments` fields; anything else there gets a hint instead of serde's bare
/// "unknown field".
/// `pbr: {"material": "steel", "params": {..}, "scale": 2}` (also `name`
/// for `material`), the shape agents reach for, lifted into the item's own
/// `pbr` / `params` / `scale`.
fn material_pbr_object(ctx: &str, body: Value) -> Result<Value, String> {
    let Value::Object(mut o) = body else {
        return Ok(body);
    };
    let Some(Value::Object(p)) = o.get("pbr").cloned() else {
        return Ok(Value::Object(o));
    };
    if let Some(k) = p
        .keys()
        .find(|k| !matches!(k.as_str(), "material" | "name" | "params" | "scale"))
    {
        return Err(format!(
            "{ctx}: pbr: unknown field `{k}`: give \"pbr\": \"<material>\" (or {{material, params?, scale?}})"
        ));
    }
    let Some(name) = p.get("material").or_else(|| p.get("name")).cloned() else {
        return Err(format!(
            "{ctx}: pbr: name the material: \"pbr\": \"<material>\" (or {{material, params?, scale?}})"
        ));
    };
    o.insert("pbr".into(), name);
    for k in ["params", "scale"] {
        if let Some(v) = p.get(k) {
            if o.contains_key(k) {
                return Err(format!(
                    "{ctx}: {k} given both in pbr and on the item: give it once"
                ));
            }
            o.insert(k.into(), v.clone());
        }
    }
    Ok(Value::Object(o))
}

fn primitive_round_object(ctx: &str, body: Value) -> Result<Value, String> {
    let Value::Object(mut o) = body else {
        return Ok(body);
    };
    match o.get("round") {
        None | Some(Value::Number(_)) => return Ok(Value::Object(o)),
        Some(Value::Object(r)) => {
            if let Some(k) = r
                .keys()
                .find(|k| !matches!(k.as_str(), "radius" | "segments"))
            {
                return Err(format!(
                    "{ctx}: round: unknown field `{k}`: {PRIMITIVE_ROUND_HINT}"
                ));
            }
            let r = r.clone();
            let Some(radius) = r.get("radius").cloned() else {
                return Err(format!(
                    "{ctx}: round: needs radius: {PRIMITIVE_ROUND_HINT}"
                ));
            };
            if let Some(seg) = r.get("segments") {
                if o.contains_key("segments") {
                    return Err(format!(
                        "{ctx}: segments given both in round and on the primitive: give it once"
                    ));
                }
                o.insert("segments".into(), seg.clone());
            }
            o.insert("round".into(), radius);
        }
        Some(other) => {
            return Err(format!(
                "{ctx}: round: got {}: {PRIMITIVE_ROUND_HINT}",
                describe_json(other)
            ));
        }
    }
    Ok(Value::Object(o))
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

const PRIMITIVE_KINDS: &[&str] = &[
    "box", "sphere", "cylinder", "cone", "capsule", "torus", "plane", "wedge", "disc", "washer",
];

pub(crate) fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != cb)).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

/// "unknown type `x`" with the closest type, or the primitive item a kind
/// name ("sphere") was meant to be.
pub fn unknown_type(other: &str) -> String {
    let low = other.to_ascii_lowercase();
    if PRIMITIVE_KINDS.contains(&low.as_str()) {
        return format!(
            "unknown type `{other}`: `{low}` is a primitive kind, write {{\"type\": \"primitive\", \"kind\": \"{low}\", ...}}"
        );
    }
    let best = OP_TYPES
        .iter()
        .map(|t| (edit_distance(&low, t), *t))
        .min()
        .filter(|(d, t)| *d <= 2.max(t.len() / 3) || t.contains(low.as_str()) && low.len() > 2);
    match best {
        Some((_, t)) => format!(
            "unknown type `{other}`; did you mean `{t}`? (one of: {})",
            OP_TYPES.join(", ")
        ),
        None => format!("unknown type `{other}` (one of: {})", OP_TYPES.join(", ")),
    }
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
            let body = primitive_round_object(&ctx, body)?;
            let s: PrimitiveSpec = de(&ctx, &body)?;
            let size = resolve_size(s.kind, &s.size.0).map_err(|e| format!("{ctx}: size: {e}"))?;
            BatchItem::Op(Op::Primitive {
                name: s.name,
                kind: s.kind,
                size,
                translation: s.translation,
                rotation: rot(&ctx, &s.rotation)?,
                round: {
                    if s.round < 0.0 || s.round.is_nan() {
                        return Err(format!("{ctx}: round: must be >= 0, got {}", s.round));
                    }
                    // Other kinds ignore it, so a style default
                    // {"primitive": {"round": "$bevel"}} works on spheres too.
                    if matches!(
                        s.kind,
                        PrimitiveKind::Box | PrimitiveKind::Cylinder | PrimitiveKind::Wedge
                    ) {
                        s.round
                    } else {
                        0.0
                    }
                },
                segments: s.segments,
                facing: match (s.facing, s.kind) {
                    (Some(_), k) if k != PrimitiveKind::Plane => {
                        return Err(format!(
                            "{ctx}: facing: only a plane can be one-sided, not a {k}"
                        ));
                    }
                    (f, _) => f,
                },
                frame: match s.frame {
                    Some(f) => {
                        if let Some((p, words)) = f.split_once(':') {
                            if p.is_empty() {
                                return Err(format!("{ctx}: frame: `{f}` names no part"));
                            }
                            // Anchor words and joint / landmark names
                            // ("eve:brow_l") both resolve on the part.
                            let _ = words;
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
                profile: expand_ring_arg(s.profile).map_err(|e| format!("{ctx}: profile: {e}"))?,
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
                    profile: expand_ring_arg(sec.profile)
                        .map_err(|e| format!("{c}: profile: {e}"))?,
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
                quads: s.quads,
            })
        }
        "panel_line" => {
            let s: PanelLineSpec = de(&ctx, &body)?;
            let def = crate::panel_line::PanelLineDef {
                input: s.input,
                view: s.view,
                points: s.points,
                closed: s.closed,
                smooth: s.smooth,
                width: s.width,
                depth: s.depth,
                cut: s.cut.unwrap_or(true),
                fill: s.fill,
            };
            def.validate().map_err(|e| format!("{ctx}: {e}"))?;
            BatchItem::Op(Op::PanelLine {
                name: s.name,
                def: Box::new(def),
            })
        }
        "cage" => {
            let s: CageSpec = de(&ctx, &body)?;
            let def = crate::cage::CageDef {
                verts: s.verts,
                faces: s.faces,
                creases: s.creases,
                mirror: s.mirror,
            };
            def.validate().map_err(|e| format!("{ctx}: {e}"))?;
            BatchItem::Op(Op::Cage {
                name: s.name,
                def: Box::new(def),
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
                follow: match s.follow.as_deref() {
                    None => String::new(),
                    Some(f @ ("upright" | "slope" | "normal")) => {
                        if s.on.is_none() {
                            return Err(format!("{ctx}: follow needs on (the surface to follow)"));
                        }
                        if f == "upright" {
                            String::new()
                        } else {
                            f.to_string()
                        }
                    }
                    Some(f) => {
                        return Err(format!(
                            "{ctx}: follow: `{f}` is not upright, slope or normal"
                        ));
                    }
                },
                on: s.on,
                between: s.between,
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
            if let Some(c) = s.cutter {
                if s.plane.is_some() {
                    return Err(format!("{ctx}: give plane or cutter, not both"));
                }
                if !["both", "inside", "outside"].contains(&keep.as_str()) {
                    return Err(format!(
                        "{ctx}: keep: with a cutter, both, inside or outside, got `{keep}`"
                    ));
                }
                if s.gap != 0.0 {
                    return Err(format!(
                        "{ctx}: gap: only for a plane (a cutter's piece moves by lift)"
                    ));
                }
                for (f, v) in [("depth", s.depth), ("lift", s.lift)] {
                    if !v.is_finite() || (f == "depth" && v < 0.0) {
                        return Err(format!(
                            "{ctx}: {f}: must be a number of meters{}",
                            if f == "depth" { " >= 0" } else { "" }
                        ));
                    }
                }
                return Ok(BatchItem::Op(Op::Split {
                    name: s.name,
                    input: s.input,
                    normal: [0.0, 1.0, 0.0],
                    point: [0.0; 3],
                    keep,
                    gap: 0.0,
                    cutter: Some(c),
                    depth: s.depth,
                    lift: s.lift,
                }));
            }
            if s.depth != 0.0 || s.lift != 0.0 {
                return Err(format!("{ctx}: depth / lift: only with a cutter"));
            }
            let Some(plane) = s.plane else {
                return Err(format!(
                    "{ctx}: give plane {{normal, point}} or cutter (a closed solid)"
                ));
            };
            if !["both", "above", "below"].contains(&keep.as_str()) {
                return Err(format!(
                    "{ctx}: keep: must be both, above or below, got `{keep}`"
                ));
            }
            let normal = plane.normal.unwrap_or([0.0, 1.0, 0.0]);
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
                point: plane.point.unwrap_or([0.0; 3]),
                keep,
                gap: s.gap,
                cutter: None,
                depth: 0.0,
                lift: 0.0,
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
                mode: s.mode.clone(),
                radius: s.radius,
                segments: s.segments,
                edges: {
                    let edges = s.edges.into_vec();
                    for e in &edges {
                        if !["all", "top", "bottom", "vertical", "horizontal"].contains(&e.as_str())
                        {
                            return Err(format!(
                                "{ctx}: edges: `{e}` is not an edge class; use all, top, bottom, vertical or horizontal"
                            ));
                        }
                    }
                    if !edges.is_empty() && s.mode != "fillet" {
                        return Err(format!("{ctx}: edges: only mode fillet takes edges"));
                    }
                    edges
                },
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
                ..Default::default()
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
                tris: s.tris,
                bake: s.bake.unwrap_or(false),
                target_tris: None,
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
                tris: s.tris,
                bake: s.bake.unwrap_or(false),
                target_tris: None,
            };
            def.validate().map_err(|e| format!("{ctx}: {e}"))?;
            BatchItem::Op(Op::Skin {
                name: s.name,
                def: Box::new(def),
            })
        }
        "body" => {
            let s: BodySpec = de(&ctx, &body)?;
            let (rig, rig_full) = match s.rig {
                None => (None, false),
                Some(RigArg::On(b)) => (Some(b), false),
                Some(RigArg::Kind(k)) if k == "game" => (None, false),
                Some(RigArg::Kind(k)) if k == "full" => (None, true),
                Some(RigArg::Kind(k)) => {
                    return Err(format!(
                        "{ctx}: rig: true, false, \"game\" or \"full\", got \"{k}\""
                    ));
                }
            };
            let def = crate::body::BodyDef {
                preset: s.preset,
                height: s.height,
                head_ratio: s.head_ratio,
                build: s.build,
                leg_length: s.leg_length,
                tail: s.tail,
                snout: s.snout,
                joints: s.joints.unwrap_or_default(),
                blend: s.blend,
                resolution: s.resolution,
                tris: s.tris,
                bake: s.bake.unwrap_or(false),
                rig,
                bind: s.bind,
                warps: s
                    .warps
                    .unwrap_or_default()
                    .into_iter()
                    .map(WarpSpec::into_deform)
                    .collect(),
                target_tris: None,
                gender: s.gender,
                age: s.age,
                muscle: s.muscle,
                weight: s.weight,
                proportions: s.proportions,
                breast_size: s.breast_size,
                breast_firmness: s.breast_firmness,
                ethnicity: s.ethnicity,
                shape: s.shape.unwrap_or_default(),
                expression: s.expression.unwrap_or_default(),
                detail: s.detail,
                parts: s.parts,
                shapes: s.shapes.unwrap_or_default(),
                rig_full,
                eye_color: s
                    .eye_color
                    .as_ref()
                    .map(parse_color)
                    .transpose()
                    .map_err(|e| format!("{ctx}: eye_color: {e}"))?,
                pupil: s.pupil,
                measure: s.measure.unwrap_or_default(),
                skin_tone: s.skin_tone,
                hair: s.hair.map(|h| *h),
                brows: s.brows.map(|h| *h),
                lashes: s.lashes.map(|h| *h),
            };
            def.validate().map_err(|e| format!("{ctx}: {e}"))?;
            BatchItem::Op(Op::Body {
                name: s.name,
                def: Box::new(def),
            })
        }
        "silhouette" => {
            let s: SilhouetteSpec = de(&ctx, &body)?;
            if s.from_reference.is_some() {
                return Err(format!(
                    "{ctx}: from_reference: traced by the server from a project image (apply_ops); give the profiles as points here"
                ));
            }
            let smooth = s.smooth == Some(true);
            let need = |v: Option<Vec<Vec<[f32; 2]>>>, f: &str| {
                v.ok_or_else(|| {
                    format!(
                        "{ctx}: {f}: required (a closed outline: side [[z, y], ...], top [[x, z], ...]), or from_reference"
                    )
                })
            };
            let ring =
                |a, f: &str| sil_rings(a, smooth, s.round).map_err(|e| format!("{ctx}: {f}: {e}"));
            let def = crate::silhouette::SilhouetteDef {
                side: need(ring(s.side, "side")?, "side")?,
                top: need(ring(s.top, "top")?, "top")?,
                front: ring(s.front, "front")?,
                blend: s.blend,
                resolution: s.resolution,
                tris: s.tris,
                bake: s.bake.unwrap_or(false),
                translation: s.translation,
                rotation: rot(&ctx, &s.rotation)?.unwrap_or([0.0, 0.0, 0.0, 1.0]),
                target_tris: None,
                references: s.references,
            };
            def.validate().map_err(|e| format!("{ctx}: {e}"))?;
            BatchItem::Op(Op::Silhouette {
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
                symmetric: s.symmetric,
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
                tris: s.tris,
                symmetric: s.symmetric,
            };
            def.validate().map_err(|e| format!("{ctx}: {e}"))?;
            BatchItem::Op(Op::Smooth {
                name: s.name,
                input: s.input,
                def: Box::new(def),
            })
        }
        "garment" => {
            let Some(obj) = body.as_object() else {
                return Err(format!("{ctx}: expected an object"));
            };
            let mut obj = obj.clone();
            let name = match obj.remove("name") {
                Some(Value::String(n)) if !n.is_empty() => n,
                _ => return Err(format!("{ctx}: name: a non-empty string is required")),
            };
            obj.remove("type");
            for k in obj.keys() {
                if !crate::garment::FIELDS.contains(&k.as_str()) {
                    return Err(format!(
                        "{ctx}: unknown field `{k}` (fields: name, {})",
                        crate::garment::FIELDS.join(", ")
                    ));
                }
            }
            let def: crate::garment::GarmentDef = de(&ctx, &Value::Object(obj))?;
            crate::garment::validate(&def).map_err(|e| format!("{ctx}: {e}"))?;
            BatchItem::Op(Op::Garment { name, def })
        }
        "stairs" | "railing" => {
            let Some(obj) = body.as_object() else {
                return Err(format!("{ctx}: expected an object"));
            };
            let mut obj = obj.clone();
            let name = match obj.remove("name") {
                Some(Value::String(n)) if !n.is_empty() => n,
                _ => return Err(format!("{ctx}: name: a non-empty string is required")),
            };
            obj.remove("type");
            let fields = if ty == "stairs" {
                crate::stairs::STAIRS_FIELDS
            } else {
                crate::stairs::RAILING_FIELDS
            };
            for k in obj.keys() {
                if !fields.contains(&k.as_str()) {
                    return Err(format!(
                        "{ctx}: unknown field `{k}` (fields: name, {})",
                        fields.join(", ")
                    ));
                }
            }
            if ty == "stairs" {
                let def: crate::stairs::StairsDef = de(&ctx, &Value::Object(obj))?;
                crate::stairs::validate_stairs(&def).map_err(|e| format!("{ctx}: {e}"))?;
                BatchItem::Op(Op::Stairs {
                    name,
                    def: Box::new(def),
                })
            } else {
                let def: crate::stairs::RailingDef = de(&ctx, &Value::Object(obj))?;
                crate::stairs::validate_railing(&def).map_err(|e| format!("{ctx}: {e}"))?;
                BatchItem::Op(Op::Railing {
                    name,
                    def: Box::new(def),
                })
            }
        }
        "carve" => {
            let Some(obj) = body.as_object() else {
                return Err(format!("{ctx}: expected an object"));
            };
            let mut obj = obj.clone();
            let name = match obj.remove("name") {
                Some(Value::String(n)) if !n.is_empty() => n,
                _ => return Err(format!("{ctx}: name: a non-empty string is required")),
            };
            obj.remove("type");
            for k in obj.keys() {
                if !crate::carve::FIELDS.contains(&k.as_str()) {
                    return Err(format!(
                        "{ctx}: unknown field `{k}` (fields: name, {})",
                        crate::carve::FIELDS.join(", ")
                    ));
                }
            }
            let def: crate::carve::CarveDef = de(&ctx, &Value::Object(obj))?;
            crate::carve::validate(&def).map_err(|e| format!("{ctx}: {e}"))?;
            BatchItem::Op(Op::Carve {
                name,
                def: Box::new(def),
            })
        }
        "shell" => {
            let s: ShellSpec = de(&ctx, &body)?;
            if s.thickness.is_nan() || s.thickness <= 0.0 {
                return Err(format!("{ctx}: thickness: must be > 0 meters"));
            }
            let open: Vec<String> = s
                .open
                .into_vec()
                .into_iter()
                .map(|w| match w.as_str() {
                    "north" => "back".to_string(),
                    "south" => "front".to_string(),
                    "east" => "right".to_string(),
                    "west" => "left".to_string(),
                    _ => w,
                })
                .collect();
            for w in &open {
                if !["top", "bottom", "left", "right", "front", "back"].contains(&w.as_str()) {
                    return Err(format!(
                        "{ctx}: open: `{w}` is not a face; use top, bottom, left, right, front or back (or north = back, south = front, east = right, west = left)"
                    ));
                }
            }
            BatchItem::Op(Op::Shell {
                name: s.name,
                input: s.input,
                thickness: s.thickness,
                open,
                method: match s.method {
                    Some(m) if m != "exact" && m != "offset" => {
                        return Err(format!("{ctx}: method: exact or offset, got `{m}`"));
                    }
                    m => m,
                },
            })
        }
        "panel" => {
            let s: PanelSpec = de(&ctx, &body)?;
            if !["top", "bottom", "left", "right", "front", "back"].contains(&s.face.as_str()) {
                return Err(format!(
                    "{ctx}: face: `{}` is not a face; use top, bottom, left, right, front or back",
                    s.face
                ));
            }
            BatchItem::Op(Op::Panel {
                name: s.name,
                input: s.input,
                face: s.face,
                rows: s.rows.unwrap_or(1),
                cols: s.cols.unwrap_or(1),
                inset: s.inset.unwrap_or(0.01),
                depth: s.depth.unwrap_or(0.005),
                gap: s.gap.unwrap_or(0.01),
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
                if b.roll.is_some_and(|r| !r.is_finite()) {
                    return Err(format!("{ctx}: bones.{n}.roll must be a number of degrees"));
                }
                let rest = b
                    .rest
                    .as_ref()
                    .map(|r| r.quat())
                    .transpose()
                    .map_err(|e| format!("{ctx}: bones.{n}.rest: {e}"))?;
                if let Some(l) = &b.limits {
                    for (ax, r) in [("x", l.x), ("y", l.y), ("z", l.z)] {
                        if let Some([lo, hi]) = r
                            && (!lo.is_finite() || !hi.is_finite() || lo > hi)
                        {
                            return Err(format!(
                                "{ctx}: bones.{n}.limits.{ax}: [min, max] degrees, min <= max"
                            ));
                        }
                    }
                }
                let look_at = match b.look_at {
                    None => None,
                    Some(la) => {
                        let lctx = format!("{ctx}: bones.{n}.look_at");
                        let axis = la
                            .axis
                            .as_ref()
                            .map(|a| signed_axis(&lctx, a))
                            .transpose()?;
                        if la.weight.is_some_and(|w| !(0.0..=1.0).contains(&w)) {
                            return Err(format!("{lctx}.weight must be 0..1"));
                        }
                        Some(crate::anim::LookAtDef {
                            target: la.target,
                            axis,
                            weight: la.weight,
                        })
                    }
                };
                bones.push(crate::anim::BoneDef {
                    name: n,
                    parent: b.parent,
                    head: b.head,
                    tail: b.tail,
                    radius: b.radius,
                    rest,
                    roll: b.roll,
                    limits: b.limits,
                    look_at,
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
                    weights_from: s.weights_from,
                    correctives: s.correctives,
                },
            })
        }
        "mocap" => {
            let s: MocapSpec = de(&ctx, &body)?;
            if s.file.is_some() == s.bvh.is_some() {
                return Err(format!(
                    "{ctx}: give `file` (a BVH file in the project, e.g. \"mocap/walk.bvh\") or inline `bvh` text"
                ));
            }
            if let Some(t) = &s.bvh {
                crate::mocap::parse(t).map_err(|e| format!("{ctx}: bvh: {e}"))?;
            }
            let mut m = crate::mocap::MocapDef {
                file: s.file,
                bvh: s.bvh,
                hash: s.hash.unwrap_or_default(),
                range: s.range,
                scale: s.scale,
                mirror: s.mirror,
                ..crate::mocap::MocapDef::default()
            };
            match &s.map {
                None => {}
                Some(Value::String(a)) if a == "auto" => {}
                Some(Value::Object(o)) => {
                    let (o, only) = match o.get("only") {
                        Some(Value::Object(x)) if o.len() == 1 => (x, true),
                        _ => (o, false),
                    };
                    m.map_only = only;
                    for (k, v) in o {
                        let Some(v) = v.as_str() else {
                            return Err(format!(
                                "{ctx}: map.{k}: a BVH joint name (\"LeftUpLeg\"), or \"\" to leave the bone unmapped"
                            ));
                        };
                        m.map.insert(k.clone(), v.to_string());
                    }
                }
                Some(other) => {
                    return Err(format!(
                        "{ctx}: map: \"auto\" or {{our bone: BVH joint}}, got {}",
                        describe_json(other)
                    ));
                }
            }
            for (f, v) in [
                ("fps", s.fps),
                ("speed", s.speed),
                ("scale", s.scale),
                ("blend", s.blend),
            ] {
                if let Some(v) = v
                    && !(v > 0.0 && v.is_finite())
                {
                    return Err(format!("{ctx}: {f} must be a positive number, got {v}"));
                }
            }
            if let Some([a, b]) = s.range
                && !(b > a && a >= 0.0)
            {
                return Err(format!(
                    "{ctx}: range [{a}, {b}]: seconds of the file, t0 < t1"
                ));
            }
            m.fps = s.fps.unwrap_or(30.0);
            m.speed = s.speed.unwrap_or(1.0);
            let looping = match &s.looping {
                None | Some(Value::Null) => false,
                Some(Value::Bool(b)) => *b,
                Some(Value::String(x)) if x == "blend" => {
                    m.blend = s.blend.unwrap_or(0.25);
                    true
                }
                Some(Value::Object(o))
                    if o.len() == 1 && o.get("blend").is_some_and(Value::is_number) =>
                {
                    m.blend = o.get("blend").and_then(Value::as_f64).unwrap_or(0.25) as f32;
                    true
                }
                Some(other) => {
                    return Err(format!(
                        "{ctx}: loop: true, false, \"blend\" or {{\"blend\": seconds}}, got {}",
                        describe_json(other)
                    ));
                }
            };
            if s.blend.is_some() && m.blend == 0.0 {
                return Err(format!("{ctx}: blend: goes with loop: \"blend\""));
            }
            m.root_motion = match &s.root_motion {
                None | Some(Value::Null) | Some(Value::Bool(false)) => None,
                Some(Value::String(x)) if x == "in_place" => None,
                Some(Value::Bool(true)) => Some("xz".into()),
                Some(Value::Object(o)) => {
                    for k in o.keys() {
                        if k != "axes" {
                            return Err(format!("{ctx}: root_motion: unknown field `{k}` (axes)"));
                        }
                    }
                    let ax = o.get("axes").and_then(Value::as_str).unwrap_or("xz");
                    if ax.is_empty() || !ax.chars().all(|c| matches!(c, 'x' | 'y' | 'z')) {
                        return Err(format!(
                            "{ctx}: root_motion axes `{ax}`: letters x, y, z (\"xz\")"
                        ));
                    }
                    Some(ax.to_string())
                }
                Some(other) => {
                    return Err(format!(
                        "{ctx}: root_motion: \"in_place\" (default), true or {{\"axes\": \"xz\"}}, got {}",
                        describe_json(other)
                    ));
                }
            };
            m.feet = plant_of(&ctx, "feet", s.feet.as_ref(), true)?;
            m.hands = plant_of(&ctx, "hands", s.hands.as_ref(), false)?;
            match &s.facing {
                None | Some(Value::Null) => {}
                Some(Value::String(x)) if x == "auto" => m.facing_auto = true,
                Some(v) if v.is_number() => m.facing = v.as_f64().unwrap_or(0.0) as f32,
                Some(other) => {
                    return Err(format!(
                        "{ctx}: facing: degrees about +y or \"auto\", got {}",
                        describe_json(other)
                    ));
                }
            }
            m.interp = match s.interp.as_deref() {
                None | Some("linear") => crate::anim::Interp::Linear,
                Some("smooth") => crate::anim::Interp::Smooth,
                Some(o) => return Err(format!("{ctx}: interp `{o}`: linear or smooth")),
            };
            BatchItem::Op(Op::Clip {
                name: s.name,
                def: crate::anim::ClipDef {
                    skeleton: s.skeleton,
                    duration: 1.0,
                    looping,
                    mocap: Some(Box::new(m)),
                    ..crate::anim::ClipDef::default()
                },
            })
        }
        "clip" => {
            let s: ClipSpec = de(&ctx, &body)?;
            let dur = match (s.duration, &s.over) {
                (Some(d), _) => {
                    if d.is_nan() || d <= 0.0 {
                        return Err(format!("{ctx}: duration must be positive (seconds)"));
                    }
                    d
                }
                // From the base clip, checked when the clip is built.
                (None, Some(_)) => f32::INFINITY,
                (None, None) => {
                    return Err(format!(
                        "{ctx}: needs duration (seconds), or `over` to take the base clip's"
                    ));
                }
            };
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
                        "rotate" | "translate" | "scale" => channel(&cctx, k, c, dur, default)?,
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
            let mut ik = std::collections::BTreeMap::new();
            for (bone, v) in &s.ik {
                ik.insert(
                    bone.clone(),
                    ik_entry(&format!("{ctx}: ik.{bone}"), v, dur, default)?,
                );
            }
            let mut poses = Vec::new();
            for (k, p) in s.poses.iter().enumerate() {
                let pctx = format!("{ctx}: poses #{k}");
                let Some([t, n]) = p
                    .as_array()
                    .map(Vec::as_slice)
                    .and_then(|a| <&[Value; 2]>::try_from(a).ok())
                else {
                    return Err(format!("{pctx}: expected [t, \"pose name\"]"));
                };
                let t = t.as_f64().ok_or(format!("{pctx}: time must be a number"))? as f32;
                if t.is_nan() || t < -1e-6 || t > dur + 1e-4 {
                    return Err(format!("{pctx}: time {t} is outside 0..{dur}"));
                }
                let Some(n) = n.as_str().filter(|n| !n.is_empty()) else {
                    return Err(format!(
                        "{pctx}: the pose is a name (a `pose` item or \"rest\")"
                    ));
                };
                poses.push((t.max(0.0), n.to_string()));
            }
            let mut events = Vec::new();
            for (k, e) in s.events.iter().enumerate() {
                let ectx = format!("{ctx}: events #{k}");
                let Some(a) = e.as_array().filter(|a| a.len() == 2 || a.len() == 3) else {
                    return Err(format!(
                        "{ectx}: expected [t, \"name\"] or [t, \"name\", data]"
                    ));
                };
                let t = a[0]
                    .as_f64()
                    .ok_or(format!("{ectx}: time must be a number"))?
                    as f32;
                if t.is_nan() || t < -1e-6 || t > dur + 1e-4 {
                    return Err(format!("{ectx}: time {t} is outside 0..{dur}"));
                }
                let Some(n) = a[1].as_str().filter(|n| !n.trim().is_empty()) else {
                    return Err(format!("{ectx}: the name must be a non-empty string"));
                };
                events.push(crate::anim::ClipEvent {
                    time: t.max(0.0),
                    name: n.to_string(),
                    data: a.get(2).cloned(),
                });
            }
            if s.over.is_none() && (s.additive || s.weight.is_some() || !s.bones.is_empty()) {
                return Err(format!(
                    "{ctx}: additive, weight and bones are for a layer: give `over` (the base clip)"
                ));
            }
            let weight = match &s.weight {
                None => None,
                Some(Value::Number(n)) => {
                    let w = n.as_f64().unwrap_or(1.0) as f32;
                    Some(crate::anim::Channel::Keys {
                        keys: vec![(0.0, [w, w, w, 0.0])],
                        interp: crate::anim::Interp::Linear,
                    })
                }
                Some(v) => Some(channel(
                    &format!("{ctx}: weight"),
                    "scale",
                    v,
                    dur,
                    default,
                )?),
            };
            let root_motion = match &s.root_motion {
                None | Some(Value::Bool(false)) => None,
                Some(Value::Bool(true)) => Some(crate::anim::RootMotionDef {
                    bone: String::new(),
                    axes: "xz".into(),
                    yaw: false,
                }),
                Some(Value::Object(o)) => {
                    for k in o.keys() {
                        if !matches!(k.as_str(), "bone" | "axes" | "yaw") {
                            return Err(format!(
                                "{ctx}: root_motion: unknown field `{k}` (bone, axes, yaw)"
                            ));
                        }
                    }
                    let axes = o.get("axes").and_then(Value::as_str).unwrap_or("xz");
                    if axes.is_empty() || !axes.chars().all(|c| matches!(c, 'x' | 'y' | 'z')) {
                        return Err(format!(
                            "{ctx}: root_motion axes `{axes}`: letters x, y, z (\"xz\")"
                        ));
                    }
                    Some(crate::anim::RootMotionDef {
                        bone: o
                            .get("bone")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        axes: axes.to_string(),
                        yaw: o.get("yaw").and_then(Value::as_bool).unwrap_or(false),
                    })
                }
                Some(other) => {
                    return Err(format!(
                        "{ctx}: root_motion: true or {{bone, axes, yaw}}, got {}",
                        describe_json(other)
                    ));
                }
            };
            let mut morphs = std::collections::BTreeMap::new();
            for (part, shapes) in &s.morphs {
                let mut m = std::collections::BTreeMap::new();
                for (shape, v) in shapes {
                    m.insert(
                        shape.clone(),
                        morph_channel(&format!("{ctx}: morphs.{part}.{shape}"), v, dur, default)?,
                    );
                }
                morphs.insert(part.clone(), m);
            }
            BatchItem::Op(Op::Clip {
                name: s.name,
                def: crate::anim::ClipDef {
                    skeleton: s.skeleton,
                    duration: if dur.is_finite() { dur } else { 0.0 },
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
                    ik,
                    interp: (!poses.is_empty()).then_some(default),
                    poses,
                    over: s.over,
                    additive: s.additive,
                    weight,
                    bones: s.bones,
                    root_motion,
                    events,
                    morphs,
                    mocap: None,
                },
            })
        }
        "pose" => {
            let s: PoseSpec = de(&ctx, &body)?;
            let mut bones = std::collections::BTreeMap::new();
            for (bone, b) in s.bones {
                let bctx = format!("{ctx}: bones.{bone}");
                let scale = match &b.scale {
                    None => None,
                    Some(v) if v.is_number() => {
                        let f = v.as_f64().unwrap_or(1.0) as f32;
                        Some([f, f, f])
                    }
                    Some(v) => Some(de::<[f32; 3]>(&format!("{bctx}.scale"), v)?),
                };
                let rotate = match &b.rotate {
                    Some(r) => Some(r.quat().map_err(|e| format!("{bctx}.rotate: {e}"))?),
                    None => None,
                };
                if rotate.is_none() && b.translate.is_none() && scale.is_none() {
                    return Err(format!("{bctx}: give rotate, translate or scale"));
                }
                bones.insert(
                    bone,
                    crate::anim::PoseBone {
                        rotate,
                        translate: b.translate,
                        scale,
                    },
                );
            }
            let mut ik = std::collections::BTreeMap::new();
            for (bone, v) in &s.ik {
                let d = ik_entry(
                    &format!("{ctx}: ik.{bone}"),
                    v,
                    1.0,
                    crate::anim::Interp::Linear,
                )?;
                if matches!(d.target, crate::anim::IkTarget::Keys(_)) {
                    return Err(format!(
                        "{ctx}: ik.{bone}: a pose's target is a point [x, y, z] or an anchor, not keys"
                    ));
                }
                ik.insert(bone.clone(), d);
            }
            if let Some((p, sh, _)) = s
                .morphs
                .iter()
                .flat_map(|(p, m)| m.iter().map(move |(sh, w)| (p, sh, w)))
                .find(|x| !x.2.is_finite())
            {
                return Err(format!("{ctx}: morphs.{p}.{sh}: the weight must be finite"));
            }
            if bones.is_empty() && ik.is_empty() && s.morphs.is_empty() {
                return Err(format!("{ctx}: give bones, ik or morphs"));
            }
            BatchItem::Op(Op::Pose {
                name: s.name,
                def: crate::anim::PoseDef {
                    skeleton: s.skeleton,
                    bones,
                    mirror: s.mirror,
                    ik,
                    morphs: s.morphs,
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
        "variant" => {
            let s: VariantSpec = de(&ctx, &body)?;
            if s.name.is_empty() || s.name == crate::variant::DEFAULT {
                return Err(format!(
                    "{ctx}: name: `{}` is the base look; name the variant (\"red_team\")",
                    s.name
                ));
            }
            if s.materials.is_empty()
                && s.style.is_none()
                && matches!(s.show, OneOrMany::None)
                && matches!(s.hide, OneOrMany::None)
            {
                return Err(format!(
                    "{ctx}: give `materials` ({{\"body*\": {{\"color\": \"#c33\"}}}}), `style` and/or `show` / `hide` part lists"
                ));
            }
            if let Some(v) = &s.style
                && !v.is_object()
            {
                return Err(format!("{ctx}: style: a token object, got {v}"));
            }
            let mut materials = Vec::new();
            for (parts, l) in s.materials {
                let at = format!("{ctx}: materials.{parts}");
                if l.pbr.is_none() && (l.params.is_some() || l.scale.is_some()) {
                    return Err(format!(
                        "{at}: params / scale apply to a pbr material; add \"pbr\": \"<material>\""
                    ));
                }
                if let Some(k) = l.scale
                    && !(k > 0.0 && k.is_finite())
                {
                    return Err(format!("{at}: scale: {k}: give a factor > 0"));
                }
                let finish = crate::material::finish::Finish {
                    bevel: l.bevel,
                    macro_: l.macro_,
                };
                finish.validate().map_err(|e| format!("{at}: {e}"))?;
                materials.push(crate::variant::Look {
                    parts,
                    base_color: l.base_color.map(|c| c.0),
                    roughness: l.roughness,
                    metalness: l.metalness,
                    pbr: l.pbr.map(|m| crate::material::PbrRef {
                        material: m,
                        params: l.params.unwrap_or_default(),
                        scale: l.scale.unwrap_or(1.0),
                        graph: None,
                    }),
                    flat: l.flat,
                    finish: (!finish.is_empty()).then_some(finish),
                });
            }
            BatchItem::Op(Op::Variant {
                name: s.name,
                materials,
                style: s.style,
                show: s.show.into_vec(),
                hide: s.hide.into_vec(),
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
            let body = material_pbr_object(&ctx, body)?;
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
            let finish = crate::material::finish::Finish {
                bevel: s.bevel,
                macro_: s.macro_,
            };
            finish.validate().map_err(|e| format!("{ctx}: {e}"))?;
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
                finish: (!finish.is_empty()).then_some(finish),
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
            if let Some(v) = &s.variant
                && v != crate::variant::DEFAULT
                && crate::variant::find(&model, v).is_none()
            {
                return Err(format!(
                    "{ctx}: variant: prefab `{}`: {}",
                    s.prefab,
                    crate::variant::unknown(&model, v)
                ));
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
                variant: s.variant.filter(|v| v != crate::variant::DEFAULT),
                components: {
                    crate::scene::check_components(&s.components)
                        .map_err(|e| format!("{ctx}: {e}"))?;
                    s.components
                },
                block: s.block,
            })
        }
        other => {
            return Err(format!("op #{i}: {}", unknown_type(other)));
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
        "panel_line" => {
            json!({"type": "panel_line", "name": "body_shut", "input": "body", "view": "right",
                "points": [[1.1, 0.35], [1.15, 0.8], [0.6, 1.05]], "smooth": true, "width": 0.006, "depth": 0.004})
        }
        "cage" => {
            json!({"type": "cage", "name": "pod_cage", "mirror": "x",
                "verts": [[0, 0, -1], [0.5, 0, -1], [0.5, 0.6, -1], [0, 0.7, -1], [0, 0, 1], [0.4, 0, 1], [0.4, 0.5, 1], [0, 0.55, 1]],
                "faces": [[0, 3, 2, 1], [4, 5, 6, 7], [1, 2, 6, 5], [2, 3, 7, 6], [0, 1, 5, 4]],
                "creases": [[1, 5, 10], [0, 1, 2]]})
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
            json!({"type": "scatter", "name": "rocks", "input": ["rock_a", "rock_b"], "on": "ground", "count": 40, "seed": 2, "scale": [0.6, 1.4], "rotate": "random", "align": 0.5, "max_slope": 35, "edge_bias": 0.3, "clump": {"count": 6, "radius": 3}})
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
        "body" => {
            json!({"type": "body", "name": "cat", "preset": "quadruped", "height": 0.25, "tail": 1.2,
                "joints": {"head": {"scale": 1.1}}, "tris": 6000})
        }
        "silhouette" => {
            json!({"type": "silhouette", "name": "car_body", "smooth": true, "blend": 0.12,
                "side": [[2.2, 0.3], [2.25, 0.75], [1.0, 0.85], [0.4, 1.4], [-1.2, 1.4], [-2.2, 0.95], [-2.2, 0.3]],
                "top": [[-0.9, 2.2], [0.9, 2.2], [0.95, -2.2], [-0.95, -2.2]],
                "front": [[-0.95, 0.3], [0.95, 0.3], [0.95, 0.9], [0.6, 1.4], [-0.6, 1.4], [-0.95, 0.9]],
                "tris": 6000})
        }
        "decimate" => {
            json!({"type": "decimate", "name": "statue", "input": "statue_hi", "ratio": 0.05, "bake": true})
        }
        "smooth" => {
            json!({"type": "smooth", "name": "pebble", "input": "pebble_cage", "mode": "subdiv", "levels": 3})
        }
        "panel" => {
            json!({"type": "panel", "name": "door", "input": "door_raw", "face": "front", "rows": 2, "cols": 1, "inset": 0.06, "depth": 0.008, "gap": 0.05})
        }
        "shell" => {
            json!({"type": "shell", "name": "carcass", "input": "carcass_raw", "thickness": 0.018, "open": "front"})
        }
        "stairs" => {
            json!({"type": "stairs", "name": "to_mezz", "from": [6.0, 0.0, 0.0], "toward": "+x", "rise": 4.4, "width": 4, "steps": 16, "run": 0.4, "collider": "blocks"})
        }
        "railing" => {
            json!({"type": "railing", "name": "mezz_rail", "path": [[-5, 4.4, 5], [5, 4.4, 5], [5, 4.4, -5]], "height": 1.0, "collider": "blocks"})
        }
        "carve" => {
            json!({"type": "carve", "name": "bunker", "cutters": ["hall", "cell", "door"], "split_by": "facing", "wall": 0.3, "markers": true, "collider": "mesh"})
        }
        "garment" => {
            json!({"type": "garment", "name": "shirt", "on": "eve", "preset": "tshirt", "region": {"neck": "v"}, "offset": 0.006})
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
            "leg_l": {"parent": "hip", "head": [0.1, 0.5, 0], "radius": 0.04, "limits": {"x": [-60, 90]}},
            "shin_l": {"parent": "leg_l", "head": [0.1, 0.27, 0.02], "radius": 0.035, "limits": {"x": [-150, 0]}},
            "foot_l": {"parent": "shin_l", "head": [0.1, 0.05, 0], "tail": [0.1, 0.0, 0.1]},
            "head": {"parent": "hip", "head": [0, 0.7, 0], "tail": [0, 0.8, 0],
                     "look_at": {"target": [0, 0.75, 2], "axis": "z", "weight": 0.8}}}}),
        "bind" => json!({"type": "bind", "name": "body_skin", "skeleton": "rig", "parts": ["body"],
            "mode": "smooth", "blend": 0.04}),
        "clip" => json!({"type": "clip", "name": "walk", "skeleton": "rig", "duration": 1.0,
            "tracks": {"hip": {"translate": {"wave": {"axis": "y", "amount": 0.02, "period": 0.5}}}},
            "ik": {"foot_l": {"target": [[0, [0.1, 0.05, 0.12]], [0.5, [0.1, 0.05, -0.12]], [1, [0.1, 0.05, 0.12]]],
                       "chain": 2, "pole": [0.1, 0.3, 1.0], "weight": 1, "rotate": [[0, {"x": 0}]]}},
            "mirror": {"foot_r": {"from": "foot_l", "phase": 0.5}},
            "poses": [[0, "rest"], [0.5, "crouch"], [1, "rest"]],
            "root_motion": {"bone": "hip", "axes": "xz"},
            "events": [[0.0, "footstep_l"], [0.5, "footstep_r", {"volume": 0.8}]]}),
        "mocap" => json!({"type": "mocap", "name": "walk_mc", "skeleton": "eve_rig",
            "file": "mocap/walk.bvh", "map": {"chest": "Spine2"}, "range": [0.0, 2.0], "fps": 30,
            "loop": "blend", "blend": 0.25, "root_motion": "in_place",
            "feet": {"plant": true, "contact_height": 0.03, "contact_speed": 0.15},
            "facing": "auto"}),
        "pose" => json!({"type": "pose", "name": "crouch", "skeleton": "rig", "mirror": true,
            "bones": {"hip": {"translate": [0, -0.3, 0]}, "thigh_l": {"rotate": {"x": -60}},
                      "spine": {"rotate": {"x": 15}}},
            "ik": {"foot_l": {"target": [0.1, 0.05, 0.1], "pole": [0.1, 0.4, 1.0]}}}),
        "sway" => json!({"type": "sway", "name": "wind", "skeleton": "oak_rig", "kind": "wind",
            "direction": [1, 0, 0.3], "strength": 0.6, "gust": 0.4, "flutter": 0.5, "duration": 4}),
        "variant" => json!({"type": "variant", "name": "red_team",
            "materials": {"body*": {"pbr": "paint", "params": {"hue": 0.0}},
                          "trim": {"color": "#c33", "flat": true}},
            "style": {"colors": {"primary": "#c33"}}}),
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
/// The prose guide for an apply_ops item type: the `docs/ops` (and shared
/// `docs/topics`) files that explain it, embedded at build time. One source
/// for AGENTS.md readers and `describe_op`, which serves it on demand so the
/// reference doesn't sit in every agent's context.
pub fn op_guide(ty: &str) -> &'static [&'static str] {
    match ty {
        "repeat" => &[include_str!("../docs/ops/repeat.md")],
        "light" => &[include_str!("../docs/ops/light.md")],
        "decal" => &[include_str!("../docs/ops/decal.md")],
        "collider" => &[include_str!("../docs/ops/../topics/game-data.md")],
        "marker" => &[include_str!("../docs/ops/../topics/game-data.md")],
        "variant" => &[include_str!("../docs/ops/variant.md")],
        "skeleton" => &[include_str!("../docs/ops/skeleton.md")],
        "bind" => &[include_str!("../docs/ops/bind.md")],
        "clip" => &[include_str!("../docs/ops/clip.md")],
        "mocap" => &[include_str!("../docs/ops/mocap.md")],
        "pose" => &[include_str!("../docs/ops/clip.md")],
        "sway" => &[include_str!("../docs/ops/sway.md")],
        "primitive" => &[include_str!("../docs/ops/primitive.md")],
        "extrude" => &[include_str!("../docs/ops/../topics/profiles.md")],
        "revolve" => &[include_str!("../docs/ops/../topics/profiles.md")],
        "group" => &[include_str!("../docs/ops/group.md")],
        "sweep" => &[
            include_str!("../docs/ops/sweep.md"),
            include_str!("../docs/ops/../topics/profiles.md"),
        ],
        "loft" => &[include_str!("../docs/ops/loft.md")],
        "cage" => &[include_str!("../docs/ops/cage.md")],
        "panel_line" => &[include_str!("../docs/ops/panel_line.md")],
        "path_array" => &[include_str!("../docs/ops/path_array.md")],
        "lsystem" => &[include_str!("../docs/ops/lsystem.md")],
        "text" => &[include_str!("../docs/ops/text.md")],
        "terrain" => &[include_str!("../docs/ops/terrain.md")],
        "hull" => &[include_str!("../docs/ops/hull.md")],
        "blob" => &[include_str!("../docs/ops/blob.md")],
        "split" => &[include_str!("../docs/ops/split.md")],
        "scatter" => &[include_str!("../docs/ops/scatter.md")],
        "round" => &[include_str!("../docs/ops/round.md")],
        "deform" => &[include_str!("../docs/ops/deform.md")],
        "sdf" => &[
            include_str!("../docs/ops/sdf.md"),
            include_str!("../docs/ops/../topics/sections.md"),
        ],
        "skin" => &[
            include_str!("../docs/ops/skin.md"),
            include_str!("../docs/ops/../topics/sections.md"),
        ],
        "body" => &[
            include_str!("../docs/ops/body.md"),
            include_str!("../docs/ops/../topics/sections.md"),
        ],
        "silhouette" => &[include_str!("../docs/ops/silhouette.md")],
        "smooth" => &[include_str!("../docs/ops/smooth.md")],
        "decimate" => &[include_str!("../docs/ops/decimate.md")],
        "shell" => &[include_str!("../docs/ops/shell.md")],
        "garment" => &[include_str!("../docs/ops/garment.md")],
        "carve" => &[include_str!("../docs/ops/carve.md")],
        "stairs" => &[include_str!("../docs/ops/stairs.md")],
        "railing" => &[include_str!("../docs/ops/stairs.md")],
        "panel" => &[include_str!("../docs/ops/panel.md")],
        "transform" => &[include_str!("../docs/ops/transform.md")],
        "move" => &[include_str!("../docs/ops/move.md")],
        "place" => &[include_str!("../docs/ops/place.md")],
        "boolean" => &[include_str!("../docs/ops/boolean.md")],
        "radial_array" => &[include_str!("../docs/ops/radial_array.md")],
        "linear_array" => &[include_str!("../docs/ops/linear_array.md")],
        "mirror" => &[include_str!("../docs/ops/mirror.md")],
        "material" => &[include_str!("../docs/ops/material.md")],
        "hide_part" => &[include_str!("../docs/ops/edits.md")],
        "show_part" => &[include_str!("../docs/ops/edits.md")],
        "rename" => &[include_str!("../docs/ops/edits.md")],
        "delete" => &[include_str!("../docs/ops/edits.md")],
        "tri_mesh" => &[include_str!("../docs/ops/edits.md")],
        "uvs" => &[include_str!("../docs/ops/edits.md")],
        "prefab" => &[include_str!("../docs/ops/prefab.md")],
        _ => &[],
    }
}

/// Topics served as `describe_op("topic:<name>")`.
pub const TOPICS: &[&str] = &[
    "animation",
    "apply-ops",
    "bsn",
    "catalog",
    "daemon",
    "export",
    "game-data",
    "history",
    "human",
    "inspect",
    "materials",
    "motion-checks",
    "profiles",
    "runtime",
    "scenes",
    "sections",
    "session",
    "style",
    "verify",
    "views",
];

/// A `docs/topics` file by name.
pub fn topic(name: &str) -> Option<&'static str> {
    Some(match name {
        "animation" => include_str!("../docs/topics/animation.md"),
        "apply-ops" => include_str!("../docs/topics/apply-ops.md"),
        "bsn" => include_str!("../docs/topics/bsn.md"),
        "catalog" => include_str!("../docs/topics/catalog.md"),
        "daemon" => include_str!("../docs/topics/daemon.md"),
        "export" => include_str!("../docs/topics/export.md"),
        "game-data" => include_str!("../docs/topics/game-data.md"),
        "history" => include_str!("../docs/topics/history.md"),
        "human" => include_str!("../docs/topics/human.md"),
        "inspect" => include_str!("../docs/topics/inspect.md"),
        "materials" => include_str!("../docs/topics/materials.md"),
        "motion-checks" => include_str!("../docs/topics/motion-checks.md"),
        "profiles" => include_str!("../docs/topics/profiles.md"),
        "runtime" => include_str!("../docs/topics/runtime.md"),
        "scenes" => include_str!("../docs/topics/scenes.md"),
        "sections" => include_str!("../docs/topics/sections.md"),
        "session" => include_str!("../docs/topics/session.md"),
        "style" => include_str!("../docs/topics/style.md"),
        "verify" => include_str!("../docs/topics/verify.md"),
        "views" => include_str!("../docs/topics/views.md"),
        _ => return None,
    })
}

pub fn describe_type(ty: Option<&str>) -> Result<String, String> {
    if let Some(t) = ty {
        if t == "nodes" {
            return crate::material::node::describe(None);
        }
        if let Some(n) = t.strip_prefix("node:") {
            return crate::material::node::describe(Some(n));
        }
        if let Some(n) = t.strip_prefix("topic:") {
            return topic(n)
                .map(str::to_string)
                .ok_or_else(|| format!("no topic `{n}` (topics: {})", TOPICS.join(", ")));
        }
    }
    let Some(ty) = ty else {
        let mut out = vec![
            "apply_ops item types (describe_op with a kind for its full schema; \"nodes\" lists material_ops node types, \"node:<type>\" gives one; \"topic:<name>\" gives a guide). Values may be \"$param\" or \"=expr\".".to_string(),
        ];
        out.push(format!("topics: {}", TOPICS.join(", ")));
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
        "cage" => schemars::schema_for!(CageSpec),
        "panel_line" => schemars::schema_for!(PanelLineSpec),
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
        "body" => schemars::schema_for!(BodySpec),
        "silhouette" => schemars::schema_for!(SilhouetteSpec),
        "smooth" => schemars::schema_for!(SmoothSpec),
        "decimate" => schemars::schema_for!(DecimateSpec),
        "shell" => schemars::schema_for!(ShellSpec),
        "garment" => schemars::schema_for!(GarmentSpec),
        "carve" => schemars::schema_for!(CarveSpec),
        "stairs" => schemars::schema_for!(StairsSpec),
        "railing" => schemars::schema_for!(RailingSpec),
        "panel" => schemars::schema_for!(PanelSpec),
        "transform" => schemars::schema_for!(TransformSpec),
        "move" => schemars::schema_for!(MoveSpec),
        "repeat" => schemars::schema_for!(RepeatSpec),
        "light" => schemars::schema_for!(LightSpec),
        "decal" => schemars::schema_for!(DecalSpec),
        "collider" => schemars::schema_for!(ColliderSpec),
        "marker" => schemars::schema_for!(MarkerSpec),
        "variant" => schemars::schema_for!(VariantSpec),
        "skeleton" => schemars::schema_for!(SkeletonSpec),
        "bind" => schemars::schema_for!(BindSpec),
        "clip" => schemars::schema_for!(ClipSpec),
        "mocap" => schemars::schema_for!(MocapSpec),
        "pose" => schemars::schema_for!(PoseSpec),
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
        // Not an apply_ops item: a material_ops node type (noise, ramp,
        // output, ...) gets its own schema instead of an "unknown" error.
        other if crate::material::node::NODE_TYPES.contains(&other) => {
            return crate::material::node::describe(Some(other)).map(|s| {
                format!("material_ops node `{other}` (apply_ops has no item of this type):\n{s}")
            });
        }
        other => {
            return Err(format!(
                "unknown item {} (for a material_ops node use \"node:<type>\"; \"nodes\" lists them)",
                unknown_type(other)
            ));
        }
    };
    let example = example(ty).unwrap_or(Value::Null);
    let guide = op_guide(ty).join("\n");
    Ok(format!(
        "{guide}\nexample:\n{}\n\nschema (plus \"type\": \"{ty}\"):\n{}",
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
        let ok = one(
            json!({"type": "shell", "name": "s", "input": "b", "thickness": 0.01, "open": ["north", "west"]}),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            matches!(ok, BatchItem::Op(Op::Shell { open, .. }) if open == ["back", "left"]),
            "compass words map to faces"
        );
        let Err(e) = one(
            json!({"type": "shell", "name": "s", "input": "b", "thickness": 0.01, "open": "up"}),
        ) else {
            panic!("bad face should fail");
        };
        assert!(e.contains("north = back"), "{e}");
        let Err(e) = one(json!({"type": "spin", "name": "x"})) else {
            panic!("unknown type should fail");
        };
        assert!(e.contains("unknown type `spin`"), "{e}");
        let Err(e) = one(json!({"type": "sphere", "name": "x"})) else {
            panic!("a kind as type should fail");
        };
        assert!(e.contains("\"kind\": \"sphere\""), "{e}");
        let Err(e) = one(json!({"type": "extrud", "name": "x"})) else {
            panic!("typo type should fail");
        };
        assert!(e.contains("did you mean `extrude`"), "{e}");
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
        ok(
            serde_json::json!({"type": "clip", "name": "c", "skeleton": "rig", "duration": 1,
            "tracks": {"elbow_l": {"rotate": {"keys": {"interp": "linear", "0": {"x": 0}, "1": {"x": 40}}}}}}),
        );
        assert!(
            parse_item(
                0,
                &serde_json::json!({"type": "clip", "name": "c", "skeleton": "rig", "duration": 1,
                "tracks": {"hip": {"translate": [[0, [0, 0, 0, 1]]]}}})
            )
            .is_err_and(|e| e.contains("[x, y, z] meters"))
        );
        ok(serde_json::json!({"type": "material", "name": "desk",
            "pbr": {"material": "steel", "params": {"rust": 0.2}, "scale": 2}}));
        assert!(
            parse_item(
                0,
                &serde_json::json!({"type": "material", "name": "d", "pbr": {"look": 1}})
            )
            .is_err_and(|e| e.contains("unknown field `look`"))
        );
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

    #[test]
    fn every_type_has_a_guide_and_every_named_topic_exists() {
        for t in OP_TYPES {
            assert!(!op_guide(t).is_empty(), "no docs/ops guide for {t}");
        }
        for t in TOPICS {
            assert!(describe_type(Some(&format!("topic:{t}"))).is_ok(), "{t}");
        }
        // Every `topic:<name>` AGENTS.md or the MCP instructions point at
        // must resolve, so a renamed file can't leave a dead pointer.
        for text in [
            include_str!("../AGENTS.md"),
            include_str!("bin/modeler/server/tools.rs"),
        ] {
            for (i, _) in text.match_indices("topic:") {
                let name: String = text[i + 6..]
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                    .collect();
                if !name.is_empty() && name != "name" {
                    assert!(topic(&name).is_some(), "dead pointer topic:{name}");
                }
            }
        }
    }

    /// AGENTS.md loads into every agent session in this repo: reference
    /// prose belongs in docs/ops and docs/topics, served by describe_op.
    #[test]
    fn agents_md_stays_small() {
        let n = include_str!("../AGENTS.md").len();
        assert!(
            n <= 32 * 1024,
            "AGENTS.md is {n} bytes; move reference prose into docs/"
        );
    }
}
