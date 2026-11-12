use crate::prelude::*;
use modeler::commands::VerifySpec;
use modeler::spec::OpSpec;
use serde_json::Value;

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct ApplyOpsArgs {
    /// Items to apply in order, each tagged with its "type".
    #[schemars(with = "Vec<OpSpec>")]
    #[serde(default)]
    pub(crate) ops: Vec<Value>,
    /// Named parameters to set before the ops run, e.g. {"tower_h": 2.0}.
    /// Any value may then be "$tower_h" or an expression "=$tower_h / 2";
    /// changing a parameter later re-derives every op that reads it.
    #[serde(default)]
    pub(crate) params: Option<std::collections::BTreeMap<String, f64>>,
    /// Optional checks to run after the batch, e.g. {"parts": [...],
    /// "bounds": {"min": [null, 0, null]}, "validate": true}. A failed check
    /// makes the call an error (the batch itself stays applied).
    #[serde(default)]
    pub(crate) expect: Option<VerifySpec>,
    /// "brief" (default): the result ends with the parts this call added /
    /// changed / removed plus warnings. "full": the whole scene summary.
    #[serde(default)]
    pub(crate) summary: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct InspectArgs {
    /// Pairs to measure: [["a", "b"], ...] gives the gap, contact or overlap
    /// (volume and depth) between two parts; ["a"] alone gives its height
    /// above the ground.
    #[serde(default)]
    pub(crate) measure: Option<Vec<Vec<String>>>,
    /// A planar cut: {"axis": "x"|"y"|"z", "at": value, "parts"?: [...]}.
    /// Returns each part's section area, outlines, holes and thinnest wall,
    /// plus an image of the cut (y = plan, z = front, x = side).
    #[serde(default)]
    pub(crate) section: Option<SectionArg>,
    /// Weigh the visible parts: {"density"?: {"part": kg/m3}, "default_density"?:
    /// 1000}. Reports mass, centre of mass, and whether it tips over (and at
    /// what tilt).
    #[serde(default)]
    pub(crate) mass: Option<MassArg>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SectionArg {
    pub(crate) axis: String,
    pub(crate) at: f32,
    #[serde(default)]
    pub(crate) parts: Option<Vec<String>>,
}

#[derive(serde::Deserialize, schemars::JsonSchema, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct MassArg {
    /// Weigh only these parts (`*` = any characters), e.g. one object of a
    /// scene; its own footprint decides stability. Default: every visible part.
    #[serde(default)]
    pub(crate) parts: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) density: std::collections::BTreeMap<String, f64>,
    #[serde(default)]
    pub(crate) default_density: Option<f64>,
}

// ---------------------------------------------------------------------------
// Other tools
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct ExportModelArgs {
    /// File path to write, e.g. "models/chair.gltf".
    pub(crate) path: String,
    /// "gltf" (default, preferred: textures as shared KTX2 files) or "glb"
    /// (one self-contained file, KTX2 textures inside). Omitted: from the
    /// path's extension, else gltf.
    #[serde(default)]
    pub(crate) format: Option<String>,
    /// "game", "none" (default), or overrides {preset?, textures?: "ktx2"
    /// (gltf default) | "png" | "embed" (glb default) | "embed_png", lods?: [0.5, 0.2],
    /// lod_distances?: [m, ..], lod_min_tris?, dedup_materials?, cull_contacts?,
    /// visible_from?: [{min, max}], view_samples?, instance?, merge?:
    /// "by_material", merge_keep?: [patterns], weld?, small_indices?, orm?,
    /// emissive_strength?}.
    #[serde(default)]
    pub(crate) optimize: Option<serde_json::Value>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct SavePrefabArgs {
    /// Human description of what the model is.
    pub(crate) description: String,
    /// Short search tags, e.g. ["furniture", "wood"].
    pub(crate) tags: Vec<String>,
    /// The prompt/idea that produced this model, for future agents.
    pub(crate) prompt: String,
    /// Overwrite the catalog entry even if it changed since this session
    /// loaded or saved it (refused otherwise: another agent saved it).
    #[serde(default)]
    pub(crate) force: Option<bool>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct NameArgs {
    /// The new name.
    pub(crate) name: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct AddAnnotationArgs {
    /// The part the comment is about ("" with `material`).
    #[serde(default)]
    pub(crate) part: String,
    /// Comment on a procedural material instead of a part (working set or
    /// library name), optionally on one of its nodes with `node`.
    pub(crate) material: Option<String>,
    pub(crate) node: Option<String>,
    /// The comment text.
    pub(crate) text: String,
    /// Optional triangle index to pin the comment to.
    pub(crate) face: Option<u32>,
    /// Optional vertex index to pin the comment to.
    pub(crate) vertex: Option<u32>,
    /// Optional world-space point [x, y, z]; defaults to the part's centroid,
    /// or the spot the human clicked when face/vertex/position are all omitted.
    pub(crate) position: Option<[f32; 3]>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct AnnotationIdArgs {
    /// The annotation id, as shown by list_annotations.
    pub(crate) id: u32,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct HistoryIdArgs {
    /// The history node id, as shown by history.
    pub(crate) id: usize,
    /// "model" (default) or "material:<name>".
    #[serde(default)]
    pub(crate) doc: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct DocArgs {
    /// "model" (default) or "material:<name>" for a material's own history.
    #[serde(default)]
    pub(crate) doc: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct UndoArgs {
    /// "model" (default) or "material:<name>" for a material's own history.
    #[serde(default)]
    pub(crate) doc: Option<String>,
    /// Undo even when the last step was made by another agent sharing this
    /// server (refused otherwise, so one agent can't silently revert another).
    #[serde(default)]
    pub(crate) force: Option<bool>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct MaterialOpsArgs {
    /// The working material to edit (created if missing).
    pub(crate) material: String,
    /// Nodes to upsert, each {"type", "name", ...} (see describe_op).
    #[serde(default)]
    pub(crate) ops: Vec<Value>,
    /// Named numbers usable as "$name" / "=expr" in node fields.
    #[serde(default)]
    pub(crate) params: Option<std::collections::BTreeMap<String, f64>>,
    /// "tile" (default) or "object".
    #[serde(default)]
    pub(crate) domain: Option<String>,
    /// Tile size in meters (tile domain; default 1).
    #[serde(default)]
    pub(crate) tile: Option<f32>,
    /// Bake resolution in texels (default 512).
    #[serde(default)]
    pub(crate) resolution: Option<u32>,
    /// Checks after the edit: {"tileable", "ranges", "normals": true,
    /// "albedo": true | [lo, hi], "coverage": [{"node", "min", "max"}]}.
    #[serde(default)]
    pub(crate) expect: Option<modeler::material::MatExpect>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct MaterialViewArgs {
    /// Material name (default: the one last edited).
    #[serde(default)]
    pub(crate) material: Option<String>,
    /// Show one node's output ("name" or "name.port") instead.
    #[serde(default)]
    pub(crate) node: Option<String>,
    /// Channel thumbnails to show (default: base_color + every driven one).
    #[serde(default)]
    pub(crate) channels: Option<Vec<String>>,
    /// Sphere image size in px (default 256).
    #[serde(default)]
    pub(crate) size: Option<usize>,
    /// Preview sphere diameter in meters (default 2 tiles, or 0.5 m).
    #[serde(default)]
    pub(crate) diameter: Option<f32>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct MaterialCatalogArgs {
    /// list, working, describe, save, load or delete.
    pub(crate) action: String,
    /// The material (save defaults to the active one).
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) description: Option<String>,
    #[serde(default)]
    pub(crate) tags: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) prompt: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct SceneArgs {
    /// list, open (an open doc, a catalog entry, or a new empty one), switch
    /// (open docs only) or close.
    pub(crate) action: String,
    /// The document (= model / prefab name).
    #[serde(default)]
    pub(crate) name: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct StyleArgs {
    /// show, set, replace, clear, list, describe, save, load or delete.
    pub(crate) action: String,
    /// For set / replace: {brief?, colors?: {name: colour}, materials?: {name:
    /// {color, roughness, metalness, pbr, params, scale}}, numbers?: {name: n},
    /// defaults?: {item_type: {field: value}}}. In set, a null value removes a token.
    #[serde(default)]
    pub(crate) tokens: Option<serde_json::Value>,
    /// The library entry (save defaults to the style's own name).
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) description: Option<String>,
    #[serde(default)]
    pub(crate) tags: Option<Vec<String>>,
    #[serde(default)]
    pub(crate) prompt: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct OpNameArgs {
    /// The op's output part name, as shown by the ops list.
    pub(crate) name: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct CatalogNameArgs {
    /// The prefab/catalog entry name.
    pub(crate) name: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct CatalogDescribeArgs {
    /// The prefab/catalog entry name.
    pub(crate) name: String,
    /// true: remove this entry from the catalog instead of describing it.
    /// The file moves to `prefabs/removed/`, so it can be restored by hand.
    #[serde(default)]
    pub(crate) delete: bool,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct SetCameraArgs {
    /// Horizontal angle in degrees: 0 = from +z (front view), 90 = from +x (right).
    pub(crate) yaw: f32,
    /// Vertical angle in degrees, -89..89.
    pub(crate) pitch: f32,
    /// Distance from the orbit target.
    pub(crate) dist: f32,
}

/// A view: a name ("front", "back", "left", "right", "top", "bottom", "iso")
/// or a camera you place.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub(crate) enum ViewArg {
    Name(String),
    Camera(CameraArg),
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CameraArg {
    /// Degrees around y: 0 = from +z (front), 90 = from +x (right), 180 = back.
    pub(crate) yaw: f32,
    /// Degrees above the horizon (-89..89); 30 is a typical three-quarter view.
    #[serde(default)]
    pub(crate) pitch: f32,
    /// Distance from the target in meters; default auto-fits the framed bounds.
    #[serde(default)]
    pub(crate) dist: Option<f32>,
    /// World point to look at; default the centre of the framed bounds.
    #[serde(default)]
    pub(crate) target: Option<[f32; 3]>,
    /// Orthographic (true proportions) instead of perspective.
    #[serde(default)]
    pub(crate) ortho: bool,
    /// Vertical field of view in degrees (perspective). Default 45.
    #[serde(default)]
    pub(crate) fov: Option<f32>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct AgentViewsArgs {
    /// 1..9 views, each a name ("front", "back", "left", "right", "top",
    /// "bottom" are orthographic and auto-framed; "iso" is perspective) or a
    /// camera {yaw, pitch, dist?, target?, ortho?, fov?}. E.g.
    /// ["front", "right", "top", "iso"] or [{"yaw": 30, "pitch": 20}, ...].
    pub(crate) views: Vec<ViewArg>,
    /// Pixel width per view. Default 1280 (640 per cell in a grid, shrunk so
    /// the whole image stays under ~1.15 MP, the size a model reads at full
    /// detail).
    #[serde(default)]
    pub(crate) width: Option<u32>,
    /// Pixel height per view. Default 720 (480 per cell in a grid).
    #[serde(default)]
    pub(crate) height: Option<u32>,
    /// Frame only these parts (hidden ones included) instead of the whole model.
    #[serde(default)]
    pub(crate) focus: Option<Vec<String>>,
    /// Tile all views into ONE labelled image (row-major). Default true when
    /// there is more than one view; false returns one image per view.
    #[serde(default)]
    pub(crate) grid: Option<bool>,
    /// "shaded" (default) or "ids": every part a flat distinct colour with a
    /// legend, to tell which shape is which part.
    #[serde(default)]
    pub(crate) style: Option<String>,
    /// Overlay the framed box's width/height and a scale bar (true scale in
    /// the orthographic views; overall size in perspective ones).
    #[serde(default)]
    pub(crate) dims: bool,
    /// "studio" (default: soft boxes + sun, shows shape) or "scene": only
    /// the model's own `light` items plus a faint ambient, to judge how the
    /// level is lit (as the game would light it).
    #[serde(default)]
    pub(crate) lighting: Option<String>,
    /// Compare with the previous render of the same view/size/style: magenta
    /// where something now is, cyan where something was, the rest greyed,
    /// with the changed share.
    #[serde(default)]
    pub(crate) diff: bool,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct SessionLogArgs {
    /// How many recent calls to show. Default 30, max 500.
    pub(crate) limit: Option<u32>,
    /// Tell the maintainers what got in your way: a call that surprised
    /// you, a workaround you needed, an error that didn't say what to do.
    /// Stored on this machine with your last few calls attached.
    #[serde(default)]
    pub(crate) feedback: Option<String>,
    /// true: instead of the call log, the friction report (failures grouped
    /// by cause, slow calls, agents' notes) from every session.
    #[serde(default)]
    pub(crate) report: bool,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct ProbeRayArgs {
    /// Ray origin, world space [x, y, z].
    pub(crate) origin: [f32; 3],
    /// Ray direction (normalized internally).
    pub(crate) dir: [f32; 3],
    /// List every part along the ray, nearest first. Default false.
    #[serde(default)]
    pub(crate) all_hits: bool,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct DiffArgs {
    /// History node id to compare from.
    pub(crate) from: usize,
    /// History node id to compare to; default the live model.
    #[serde(default)]
    pub(crate) to: Option<usize>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct CherryPickArgs {
    /// History node to take ops from (e.g. the tip of a branch you jumped away from).
    pub(crate) from: usize,
    /// Only these op output names; default every op there that is new or
    /// different from the live graph.
    #[serde(default)]
    pub(crate) ops: Option<Vec<String>>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct DescribeOpArgs {
    /// An apply_ops item type (primitive, transform, ...); omit to list all
    /// types with an example each.
    #[serde(default)]
    pub(crate) kind: Option<String>,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct HistoryPruneArgs {
    /// How many of the newest snapshots to keep (cursor, root and branch
    /// points are always kept as well).
    pub(crate) keep: usize,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct HistoryArgs {
    /// Show only the newest N nodes (plus the cursor). Default 30.
    #[serde(default)]
    pub(crate) limit: Option<usize>,
    /// "model" (default) or "material:<name>".
    #[serde(default)]
    pub(crate) doc: Option<String>,
}
