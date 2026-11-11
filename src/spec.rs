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
    /// Optional rotation, quaternion [x, y, z, w].
    #[serde(default)]
    rotation: Option<[f32; 4]>,
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
    /// Quaternion [x, y, z, w].
    #[serde(default)]
    rotation: Option<[f32; 4]>,
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
    /// Quaternion [x, y, z, w].
    #[serde(default)]
    rotation: Option<[f32; 4]>,
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
    /// World points [[x, y, z], ...] the profile travels along. Use many
    /// points for curves (bends are mitred to keep thickness).
    path: Vec<[f32; 3]>,
    /// Join the end back to the start (a ring); otherwise both ends are capped.
    #[serde(default)]
    closed: bool,
    /// Round the profile's corners with this radius.
    #[serde(default)]
    round: f32,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HullSpec {
    name: String,
    /// Parts whose convex hull to take (e.g. two spheres = a capsule between
    /// them; eight small spheres = a rounded box). First input's material.
    inputs: Vec<String>,
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
    /// Quaternion [x, y, z, w].
    #[serde(default)]
    rotation: Option<[f32; 4]>,
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
    /// Quaternion [x, y, z, w]; or give axis + degrees instead.
    #[serde(default)]
    rotation: Option<[f32; 4]>,
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
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PartSpec {
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
    /// Quaternion [x, y, z, w].
    #[serde(default)]
    rotation: Option<[f32; 4]>,
    #[serde(default)]
    scale: Option<[f32; 3]>,
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
    Hull(HullSpec),
    Round(RoundSpec),
    Shell(ShellSpec),
    Transform(TransformSpec),
    Move(MoveSpec),
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
    "primitive",
    "extrude",
    "revolve",
    "group",
    "sweep",
    "hull",
    "round",
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
                rotation: s.rotation,
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
                rotation: s.rotation,
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
                rotation: s.rotation,
            })
        }
        "sweep" => {
            let s: SweepSpec = de(&ctx, &body)?;
            BatchItem::Op(Op::Sweep {
                name: s.name,
                profile: expand_ring(s.profile).map_err(|e| format!("{ctx}: profile: {e}"))?,
                path: s.path,
                closed: s.closed,
                round: s.round,
            })
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
                rotation: s.rotation,
                scale: s.scale,
                relative: s.relative,
                pivot: s.pivot,
            })
        }
        "move" => {
            let s: MoveSpec = de(&ctx, &body)?;
            let rotation = match (s.rotation, s.axis, s.degrees) {
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
            }
        }
        "place" => {
            let s: PlaceSpec = de(&ctx, &body)?;
            if s.parts.is_empty() {
                return Err(format!("{ctx}: parts is empty"));
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
            BatchItem::Op(Op::Material {
                input: s.input.unwrap_or_else(|| s.name.clone()),
                name: s.name,
                base_color: s.base_color.map(|c| c.0),
                roughness: s.roughness,
                metalness: s.metalness,
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
                crate::catalog::load_prefab(&s.prefab).map_err(|e| format!("{ctx}: {e}"))?;
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
                rotation: s.rotation,
                scale: s.scale,
                parts,
                digest,
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
        "hull" => {
            json!({"type": "hull", "name": "pill", "inputs": ["ball_a", "ball_b"]})
        }
        "round" => {
            json!({"type": "round", "name": "table_top_r", "input": "table_top", "mode": "fillet", "radius": 0.01})
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
    let Some(ty) = ty else {
        let mut out = vec![
            "apply_ops item types (describe_op with a kind for its full schema). Values may be \"$param\" or \"=expr\".".to_string(),
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
        "hull" => schemars::schema_for!(HullSpec),
        "round" => schemars::schema_for!(RoundSpec),
        "shell" => schemars::schema_for!(ShellSpec),
        "transform" => schemars::schema_for!(TransformSpec),
        "move" => schemars::schema_for!(MoveSpec),
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
    fn every_type_has_a_parseable_example() {
        for t in OP_TYPES {
            let e = example(t).unwrap_or_else(|| panic!("no example for {t}"));
            if *t == "prefab" {
                continue; // needs a catalog entry
            }
            parse_item(0, &e).unwrap_or_else(|err| panic!("{t}: {err}"));
            describe_type(Some(t)).unwrap_or_else(|err| panic!("{t}: {err}"));
        }
        assert!(describe_type(None).is_ok_and(|s| s.contains("radial_array")));
    }
}
