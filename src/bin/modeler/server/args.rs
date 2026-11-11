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
}

// ---------------------------------------------------------------------------
// Other tools
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct ExportModelArgs {
    /// File path to write, e.g. "chair.glb".
    pub(crate) path: String,
    /// "gltf" for a JSON .gltf or "glb" for a binary .glb.
    pub(crate) format: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct SavePrefabArgs {
    /// Human description of what the model is.
    pub(crate) description: String,
    /// Short search tags, e.g. ["furniture", "wood"].
    pub(crate) tags: Vec<String>,
    /// The prompt/idea that produced this model, for future agents.
    pub(crate) prompt: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct NameArgs {
    /// The new name.
    pub(crate) name: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct AddAnnotationArgs {
    /// The part the comment is about.
    pub(crate) part: String,
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
pub(crate) struct SetCameraArgs {
    /// Horizontal angle in degrees (0 looks down -z).
    pub(crate) yaw: f32,
    /// Vertical angle in degrees, -89..89.
    pub(crate) pitch: f32,
    /// Distance from the orbit target.
    pub(crate) dist: f32,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct AgentViewsArgs {
    /// Views: "front", "back", "left", "right", "top", "bottom" (orthographic,
    /// auto-framed) or "iso" (perspective). E.g. ["front", "right", "top"] or
    /// ["iso"]. Each returns its own image.
    pub(crate) views: Vec<String>,
    /// Pixel width per view. Default 1280.
    #[serde(default = "default_shot_width")]
    pub(crate) width: u32,
    /// Pixel height per view. Default 720.
    #[serde(default = "default_shot_height")]
    pub(crate) height: u32,
    /// Frame only these parts (hidden ones included) instead of the whole model.
    #[serde(default)]
    pub(crate) focus: Option<Vec<String>>,
}

pub(crate) fn default_shot_width() -> u32 {
    1280
}

pub(crate) fn default_shot_height() -> u32 {
    720
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub(crate) struct SessionLogArgs {
    /// How many recent calls to show. Default 30, max 500.
    pub(crate) limit: Option<u32>,
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
