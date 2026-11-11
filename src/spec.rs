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
/// 0..255, or a hex string "#rrggbb" / "#rrggbbaa".
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
            "description": "Colour: [r, g, b] or [r, g, b, a] (0..1, or 0..255), or \"#rrggbb\" / \"#rrggbbaa\".",
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
            if !(h.len() == 6 || h.len() == 8) || !h.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(format!(
                    "colour string must be #rrggbb or #rrggbbaa, got {s:?}"
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
    /// box, planar, cylindrical or spherical.
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
    Transform(TransformSpec),
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
    "transform",
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
        "transform" => {
            let s: TransformSpec = de(&ctx, &body)?;
            BatchItem::Op(Op::Transform {
                name: s.name,
                input: s.input,
                translation: s.translation,
                rotation: s.rotation,
                scale: s.scale,
                relative: s.relative,
            })
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
}
