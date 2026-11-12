//! The tool operations as plain functions on a [`Session`] (model + undo history). Pure,
//! unit-testable, wiring-free API used by both the MCP tools and tests.

use crate::Vec3;
use crate::{Annotation, Axis, BoolOp, Model, ModelerError, Op, Part, PrimitiveKind, graph};
use std::collections::HashMap;
use std::sync::Arc;

pub type CmdResult = Result<String, ModelerError>;

/// One item in an `apply_batch` call: either a graph op or a baked edit
/// (hide/show/rename) that runs in batch position.
#[derive(Debug)]
pub enum BatchItem {
    Op(Op),
    Hide {
        part: String,
    },
    Show {
        part: String,
    },
    Rename {
        part: String,
        to: String,
    },
    /// Remove the op producing `name` (or a baked part of that name).
    Delete {
        name: String,
    },
    /// A baked raw-triangle part.
    TriMesh {
        name: String,
        verts: Vec<[f32; 3]>,
        indices: Vec<u32>,
        normals: Option<Vec<[f32; 3]>>,
        uvs: Option<Vec<[f32; 2]>>,
    },
    /// Re-project a part's UVs (baked).
    Uvs {
        part: String,
        projection: String,
    },
    /// One rigid relative transform over many parts (names / `*` globs);
    /// with `on`, a `place` (drop onto that part's top surface).
    Move {
        name: Option<String>,
        parts: Vec<String>,
        translation: Option<[f32; 3]>,
        rotation: Option<[f32; 4]>,
        pivot: Option<PivotSpec>,
        on: Option<String>,
        at: Option<[f32; 2]>,
        gap: Option<f32>,
        toward: Option<String>,
        each: bool,
    },
}

/// A move's pivot: a point, or an anchor on a bounding box —
/// `"bottom-back"` (the moved parts' own box) or `"laptop_screen:bottom-front"`
/// (another part's box; `*` patterns allowed). Words: left/right (x),
/// bottom/top (y), back/front (z), center; unnamed axes stay centred.
#[derive(Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum PivotSpec {
    Point([f32; 3]),
    Anchor(String),
}

/// Visible parts on the default material, and those with only a flat
/// colour (no procedural `pbr` material, and not marked `flat: true`).
pub fn material_gaps(model: &Model) -> (Vec<String>, Vec<String>) {
    let (mut bare, mut flat) = (Vec::new(), Vec::new());
    for p in model.parts.iter().filter(|p| p.visible) {
        if p.material.is_default() {
            bare.push(p.name.clone());
        } else if !p.material.has_pbr() && !p.material.flat {
            flat.push(p.name.clone());
        }
    }
    (bare, flat)
}

/// A name list cut to 12 with a count of the rest.
fn fold(names: &[String]) -> String {
    let mut s = names
        .iter()
        .take(12)
        .map(|n| format!("`{n}`"))
        .collect::<Vec<_>>()
        .join(", ");
    if names.len() > 12 {
        s += &format!(", +{} more", names.len() - 12);
    }
    s
}

/// Resolves an anchor's words against a box.
pub fn anchor_point(words: &str, lo: Vec3, hi: Vec3) -> Result<[f32; 3], String> {
    let mut f = [0.5f32; 3];
    for w in words.split(['-', ' ', ',', '_']).filter(|w| !w.is_empty()) {
        match w {
            "left" => f[0] = 0.0,
            "right" => f[0] = 1.0,
            "bottom" => f[1] = 0.0,
            "top" => f[1] = 1.0,
            "back" => f[2] = 0.0,
            "front" => f[2] = 1.0,
            "center" | "centre" | "middle" => {}
            other => {
                return Err(format!(
                    "anchor word `{other}`: use left/right, bottom/top, back/front, center"
                ));
            }
        }
    }
    Ok([
        lo.x + (hi.x - lo.x) * f[0],
        lo.y + (hi.y - lo.y) * f[1],
        lo.z + (hi.z - lo.z) * f[2],
    ])
}

/// Deterministic checks for `verify` and an `apply_ops` expect block. Each
/// field is optional; only the given checks run.
#[derive(Clone, Debug, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VerifySpec {
    /// Exact visible part names (order-insensitive); a trailing `*` matches
    /// a prefix (`"gz.*"`).
    #[serde(default)]
    pub parts: Option<Vec<String>>,
    /// Names that must be visible parts; extras are fine. Trailing `*` = prefix.
    #[serde(default)]
    pub includes: Option<Vec<String>>,
    /// Every visible part reaches the ground through touching parts. The
    /// result notes parts held up only by passing deep (> 20%) into another;
    /// `"strict"` fails them.
    #[serde(default)]
    pub grounded: Option<GroundedSpec>,
    /// Every visible part has a material: `true` fails parts on the default
    /// material, `"pbr"` also fails flat colours (a procedural `pbr`
    /// material required).
    #[serde(default)]
    pub materials: Option<GroundedSpec>,
    /// Limit the checks to these parts (`*` globs): grounded / supported /
    /// validate / bounds / stable report only them (grounding still uses
    /// every part), collisions keep pairs with at least one side in scope.
    #[serde(default)]
    pub scope: Option<Vec<String>>,
    /// World bounds of visible parts; `null` entries skip an axis.
    #[serde(default)]
    pub bounds: Option<BoundsSpec>,
    /// Mesh checks: every visible part closed and outward-wound.
    #[serde(default)]
    pub validate: Option<bool>,
    /// Re-derive the graph uncached and diff against the cached parts.
    #[serde(default)]
    pub rebuild: Option<bool>,
    /// The centre of mass sits over the footprint (the model does not tip
    /// over), with uniform density.
    #[serde(default)]
    pub stable: Option<bool>,
    /// Nothing hangs: every visible part is on the ground, shares volume
    /// with another part (a joint), or touches another BELOW its centre of
    /// mass. Fails a part only wedged against something above it.
    #[serde(default)]
    pub supported: Option<bool>,
    /// Pairs `[a, b]`: `a` rests on (or is attached to) `b`.
    #[serde(default)]
    pub rests: Option<Vec<[String; 2]>>,
    /// No part passes through another: `true`, or `{allow: [[a, b]], max}`.
    /// Fails any pair where more than `max` (default 0.2) of the smaller
    /// part's volume is inside the other, unless `allow` lists the pair
    /// (`*` patterns, either order). Shallow overlaps are joints.
    #[serde(default)]
    pub collisions: Option<CollisionSpec>,
    /// The checks wrapped as in `apply_ops`' `expect` (`{"expect": {...}}`):
    /// read as if given directly.
    #[serde(default)]
    #[schemars(skip)]
    pub expect: Option<Box<VerifySpec>>,
    /// No visible part's surface passes through itself (a deform, skin or
    /// sdf warp that folded it): fails with the crossing count and where.
    #[serde(default)]
    pub self_intersections: Option<bool>,
    /// Every visible part is at least this many meters thick everywhere
    /// (measured straight through along the inward normal): catches limbs
    /// pinched to nothing, walls too thin to print or cast.
    #[serde(default)]
    pub thickness: Option<f32>,
    /// Smooth-bound parts have every vertex weighted (sums 1, <= 4 bones).
    #[serde(default)]
    pub weights: Option<bool>,
    /// Checks across a clip's samples: loop seam, edge stretch, `floor`,
    /// `collisions` (`{"clip": "walk", "floor": 0.005, "collisions": true}`).
    #[serde(default)]
    pub animation: Option<crate::anim::check::AnimationSpec>,
}

/// `grounded: true` or `"strict"`.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum GroundedSpec {
    On(bool),
    Mode(String),
}

/// `collisions: true` or `{allow, max}`.
#[derive(Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum CollisionSpec {
    On(bool),
    With {
        /// `[a, b]` pairs (`*` globs, either order), or `{"pair": [a, b],
        /// "depth": 0.003, "max": 0.5}` to allow only shallow overlaps.
        #[serde(default)]
        allow: Vec<crate::contact::AllowRule>,
        #[serde(default)]
        max: Option<f64>,
    },
}

#[derive(Clone, Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BoundsSpec {
    #[serde(default)]
    pub min: [Option<f32>; 3],
    #[serde(default)]
    pub max: [Option<f32>; 3],
    /// Allowed deviation per checked axis. Default 0.01.
    #[serde(default = "default_bounds_tol")]
    pub tol: f32,
}

fn default_bounds_tol() -> f32 {
    0.01
}

/// A spot in the live view a human clicked on, waiting for its comment text. Set by
/// the preview app; the next all-optional `add_annotation` consumes it.
#[derive(Clone, Debug)]
pub struct PendingPin {
    pub part: String,
    pub face: Option<u32>,
    pub vertex: Option<u32>,
    pub position: [f32; 3],
}

/// The modeling session the MCP tools drive.
pub struct Session {
    pub model: Model,
    /// The working procedural materials (each with its own history tree).
    pub materials: crate::material::Library,
    /// The human's last click on the model, if it hasn't been commented yet.
    pub pending_pin: Option<PendingPin>,
    /// Nesting depth of [`Session::batch`]; while > 0, commits are collected
    /// into one history step recorded when the outermost batch ends.
    batch_depth: usize,
    batch_labels: Vec<String>,
    /// Open documents other than the active `model` (see [`Session::scene`]).
    pub others: Vec<Model>,
    /// Unsaved documents a restarted server found in the autosave: kept
    /// (`parked_docs.json`) but not open until an agent asks with
    /// `scene restore`, so a restart never brings back stale copies on its
    /// own.
    pub parked: Vec<Model>,
    /// The document the human picked in the window (popup, grid, outliner).
    /// Agent calls switch to their own documents and back to this one when
    /// they end, so agents never change what the human is looking at.
    /// `None` until the human picks one: the window then shows whatever the
    /// last call worked on.
    pub viewer_doc: Option<String>,
    /// Agent calls begun and not yet ended (calls overlap while one
    /// renders), and the document each in-flight client works in.
    in_flight: usize,
    call_doc: HashMap<String, String>,
    /// Bumped by every comment edit (they don't bump the model version), so
    /// the window redraws its comments and pins.
    pub annotation_rev: u64,
    /// Autosaves waiting to be written (see [`Session::flush_autosave`]).
    pub autosave_due: AutosaveDue,
    /// How much of the history the autosave journal already holds.
    pub journal: crate::catalog::JournalState,
    /// Who is calling right now, when the call says (the driver daemon tags
    /// every call with its agent): history nodes and op writes are credited
    /// to it, and `undo` won't revert another client's edit unasked.
    pub client: Option<String>,
    /// Per client: its own active document, and how far it has seen that
    /// document's history (node count, cursor), so each agent keeps its
    /// document and hears about other agents' edits to it.
    clients: HashMap<String, ClientView>,
    /// Per (document, op template key): the client that last wrote the op.
    op_authors: HashMap<(String, String), String>,
    /// Per document name: the catalog file's fingerprint when this session
    /// loaded or saved it. `save_prefab` refuses to overwrite a file that
    /// changed since (another agent or server saved it) unless forced.
    pub catalog_base: HashMap<String, u64>,
    /// Per document: its history (node count, cursor) when it last matched
    /// its catalog file (saved, or opened from the catalog). A document
    /// still in that state loses nothing by closing.
    saved_state: HashMap<String, (usize, Option<usize>)>,
    /// Per document: when a call last worked in it (documents not touched
    /// since this server started count from `started`).
    touched: HashMap<String, std::time::Instant>,
    started: std::time::Instant,
    idle_checked: Option<std::time::Instant>,
    /// Restored documents found to differ from their catalog file.
    unsaved_checked: std::collections::HashSet<String>,
    /// Set when an agent the server hasn't seen makes its first call: the
    /// window comes to the front so the human sees the new work.
    pub focus_requested: bool,
}

impl Session {
    /// Every client (agent) this server has seen, with its active document.
    pub fn clients(&self) -> impl Iterator<Item = (&str, &str)> {
        self.clients
            .iter()
            .map(|(c, v)| (c.as_str(), v.doc.as_str()))
    }

    /// When a call last worked in document `doc` (None: not this run).
    pub fn last_touched(&self, doc: &str) -> Option<std::time::Instant> {
        self.touched.get(doc).copied()
    }

    /// Whether document `name` (open, active or not) matches its catalog file.
    pub fn is_saved_doc(&self, name: &str) -> bool {
        let model = if self.model.name == name {
            Some(&self.model)
        } else {
            self.others.iter().find(|m| m.name == name)
        };
        // An empty document (the placeholder a restart starts on) has
        // nothing to lose.
        model.is_some_and(|m| {
            (m.ops.is_empty() && m.parts.is_empty())
                || self.saved_state.get(name) == Some(&(m.history.nodes.len(), m.history.current))
        })
    }
}

/// "the shared library" or "project `name`" for messages.
pub fn project_label(p: Option<&std::path::Path>) -> String {
    match p {
        None => "the shared library".into(),
        Some(p) => format!(
            "project `{}`",
            p.file_name()
                .map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into())
        ),
    }
}

/// Saved, unchanged documents nobody has worked in for this long are closed
/// (they reopen from the catalog on demand), so a long multi-agent session
/// doesn't pile up hundreds of open documents.
pub const IDLE_CLOSE: std::time::Duration = std::time::Duration::from_secs(20 * 60);

#[derive(Clone, Debug, Default)]
struct ClientView {
    doc: String,
    /// Already told that its document is kept for it (said once).
    told_switch: bool,
    seen_nodes: usize,
    seen_cursor: Option<usize>,
}

/// Which autosaves are pending and since when: edits only mark them, and
/// the app writes them once edits pause, so serializing the history never
/// sits on a tool call's path.
#[derive(Default, Debug)]
pub struct AutosaveDue {
    pub model: bool,
    pub docs: bool,
    pub materials: bool,
    /// When the latest pending edit happened.
    pub since: Option<std::time::Instant>,
}

/// How long autosaves wait after the last edit before writing.
pub const AUTOSAVE_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

impl Default for Session {
    fn default() -> Self {
        Self {
            model: Model::named("model"),
            materials: Default::default(),
            pending_pin: None,
            batch_depth: 0,
            batch_labels: Vec::new(),
            others: Vec::new(),
            parked: Vec::new(),
            viewer_doc: None,
            in_flight: 0,
            call_doc: HashMap::new(),
            annotation_rev: 0,
            autosave_due: AutosaveDue::default(),
            journal: Default::default(),
            client: None,
            clients: HashMap::new(),
            op_authors: HashMap::new(),
            catalog_base: HashMap::new(),
            saved_state: HashMap::new(),
            touched: HashMap::new(),
            started: std::time::Instant::now(),
            idle_checked: None,
            unsaved_checked: Default::default(),
            focus_requested: false,
        }
    }
}

/// What a move did, in words: where the moved parts' centre went, parts it
/// left behind (they touched a moved part and now touch nothing), and new
/// overlaps between moved and unmoved parts.
fn move_report(before: &[Arc<Part>], after: &[Arc<Part>], patterns: &[String]) -> String {
    use crate::contact::{Body, contact};
    let moved = |p: &Part| patterns.iter().any(|q| crate::name_matches(q, &p.name));
    let live = |ps: &[Arc<Part>]| -> Vec<Arc<Part>> {
        ps.iter()
            .filter(|p| p.visible && !p.verts.is_empty() && !p.indices.is_empty())
            .cloned()
            .collect()
    };
    let (before, after) = (live(before), live(after));
    let centre = |ps: &[Arc<Part>]| {
        ps.iter()
            .filter(|p| moved(p))
            .map(|p| p.bounds())
            .reduce(|(a, b), (c, d)| (a.min(c), b.max(d)))
            .map(|(lo, hi)| (lo + hi) * 0.5)
    };
    let mut out = String::new();
    if let (Some(c0), Some(c1)) = (centre(&before), centre(&after)) {
        let d = c1 - c0;
        let words: Vec<String> = [
            (d.x, "right (+x)", "left (-x)"),
            (d.y, "up (+y)", "down (-y)"),
            (d.z, "front (+z)", "back (-z)"),
        ]
        .iter()
        .filter(|(v, _, _)| v.abs() >= 1e-3)
        .map(|(v, pos, neg)| format!("{:.3} m {}", v.abs(), if *v > 0.0 { pos } else { neg }))
        .collect();
        out += &if words.is_empty() {
            "\n  centre stays put".to_string()
        } else {
            format!("\n  centre moved {}", words.join(", "))
        };
    }
    // Only parts near the moved set, before or after, can change contact.
    // Bounds once per part, not per comparison.
    let moved_boxes = |set: &[Arc<Part>]| -> Vec<(Vec3, Vec3)> {
        set.iter()
            .filter(|m| moved(m))
            .map(|m| m.bounds())
            .collect()
    };
    let (mb0, mb1) = (moved_boxes(&before), moved_boxes(&after));
    let near_any = |lo: Vec3, hi: Vec3, boxes: &[(Vec3, Vec3)]| {
        boxes
            .iter()
            .any(|(a, b)| (0..3).all(|k| lo[k] <= b[k] + 1e-3 && a[k] <= hi[k] + 1e-3))
    };
    // Each part's solid is built once, on first use (the old loop rebuilt
    // one per pair, and every part's for each "left behind" test).
    let bodies = |ps: &[Arc<Part>]| -> Vec<std::cell::OnceCell<Body<'_>>> {
        ps.iter().map(|_| std::cell::OnceCell::new()).collect()
    };
    let (b0, b1) = (bodies(&before), bodies(&after));
    let idx = |ps: &[Arc<Part>], n: &str| ps.iter().position(|p| p.name == n);
    let get0 = |i: usize| b0[i].get_or_init(|| Body::new(&before[i]));
    let get1 = |i: usize| b1[i].get_or_init(|| Body::new(&after[i]));
    let boxes1: Vec<(Vec3, Vec3)> = after.iter().map(|p| p.bounds()).collect();
    let close = |a: usize, b: usize| {
        let ((alo, ahi), (blo, bhi)) = (boxes1[a], boxes1[b]);
        (0..3).all(|k| alo[k] <= bhi[k] + 1e-3 && blo[k] <= ahi[k] + 1e-3)
    };
    let moved0: Vec<usize> = (0..before.len()).filter(|&i| moved(&before[i])).collect();
    let moved1: Vec<usize> = (0..after.len()).filter(|&i| moved(&after[i])).collect();
    let mut left = Vec::new();
    let mut new_ov = Vec::new();
    for (ui, u) in after.iter().enumerate().filter(|(_, p)| !moved(p)) {
        let (lo, hi) = boxes1[ui];
        if !near_any(lo, hi, &mb0) && !near_any(lo, hi, &mb1) {
            continue;
        }
        let Some(u0i) = idx(&before, &u.name) else {
            continue;
        };
        let touched_moved = moved0
            .iter()
            .any(|&m| contact(get0(u0i), get0(m)).is_some());
        if touched_moved
            && (0..after.len())
                .filter(|&o| o != ui && close(ui, o))
                .all(|o| contact(get1(ui), get1(o)).is_none())
        {
            left.push(u.name.clone());
        }
        for &mi in &moved1 {
            let Some(m0i) = idx(&before, &after[mi].name) else {
                continue;
            };
            let was = crate::contact::overlap_between(get0(u0i), get0(m0i)).is_some();
            if !was && let Some(o) = crate::contact::overlap_between(get1(ui), get1(mi)) {
                new_ov.push(o);
            }
        }
    }
    if !left.is_empty() {
        out += &format!(
            "\n  WARNING left behind: {} touched a moved part and now touches nothing (add it to `parts` to carry it along)",
            left.iter()
                .map(|n| format!("`{n}`"))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    if !new_ov.is_empty() {
        new_ov.sort_by(|x, y| y.fraction.total_cmp(&x.fraction));
        out += &format!(
            "\n  WARNING new overlap(s): {}",
            new_ov
                .iter()
                .map(|o| o.describe())
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    out
}

/// Stable reorder so an op that reads a part produced by a LATER op in the
/// same batch runs after it (item order inside a batch doesn't matter).
/// In-place edits (output == input) never count as producers; non-op items
/// keep their place relative to each other. Edits that name existing parts
/// (hide/show, rename, uvs, move/place) run after every op, in the order
/// written, so they see every part the batch creates and a later
/// `show_part x` still overrides an earlier `hide_part "*_raw"`.
fn producers_first<T>(items: Vec<(BatchItem, T)>) -> Vec<(BatchItem, T)> {
    let late = |it: &BatchItem| {
        matches!(
            it,
            BatchItem::Hide { .. }
                | BatchItem::Show { .. }
                | BatchItem::Rename { .. }
                | BatchItem::Uvs { .. }
                | BatchItem::Move { .. }
        )
    };
    let (tail, head): (Vec<_>, Vec<_>) = items.into_iter().partition(|(it, _)| late(it));
    let mut out = order_producers(head);
    out.extend(places_after_their_support(tail));
    out
}

/// Stable reorder of the late items so a place runs after any move/place
/// in the batch that moves the part it lands on.
fn places_after_their_support<T>(items: Vec<(BatchItem, T)>) -> Vec<(BatchItem, T)> {
    let n = items.len();
    let deps: Vec<Vec<usize>> = (0..n)
        .map(|i| {
            let BatchItem::Move { on: Some(on), .. } = &items[i].0 else {
                return Vec::new();
            };
            (0..n)
                .filter(|&j| {
                    j != i
                        && matches!(&items[j].0, BatchItem::Move { parts, .. }
                            if parts.iter().any(|p| crate::name_matches(p, on)))
                })
                .collect()
        })
        .collect();
    if deps.iter().all(Vec::is_empty) {
        return items;
    }
    let mut state = vec![0u8; n];
    let mut order = Vec::with_capacity(n);
    fn visit(i: usize, deps: &[Vec<usize>], state: &mut [u8], order: &mut Vec<usize>) {
        if state[i] != 0 {
            return;
        }
        state[i] = 1;
        for &j in &deps[i] {
            visit(j, deps, state, order);
        }
        state[i] = 2;
        order.push(i);
    }
    for i in 0..n {
        visit(i, &deps, &mut state, &mut order);
    }
    let mut slots: Vec<Option<(BatchItem, T)>> = items.into_iter().map(Some).collect();
    order.into_iter().filter_map(|i| slots[i].take()).collect()
}

fn order_producers<T>(items: Vec<(BatchItem, T)>) -> Vec<(BatchItem, T)> {
    let n = items.len();
    let produces = |it: &BatchItem| -> Vec<String> {
        match it {
            BatchItem::Op(op) if !graph::inputs(op).contains(&graph::output(op)) => {
                graph::outputs(op)
            }
            _ => Vec::new(),
        }
    };
    let deps: Vec<Vec<usize>> = (0..n)
        .map(|i| {
            let BatchItem::Op(op) = &items[i].0 else {
                return Vec::new();
            };
            let ins = graph::inputs(op);
            ((i + 1)..n)
                .filter(|&j| {
                    produces(&items[j].0)
                        .iter()
                        .any(|o| ins.iter().any(|p| crate::name_matches(p, o)))
                })
                .collect()
        })
        .collect();
    if deps.iter().all(Vec::is_empty) {
        return items;
    }
    let mut order = Vec::with_capacity(n);
    let mut done = vec![false; n];
    fn visit(
        i: usize,
        deps: &[Vec<usize>],
        done: &mut [bool],
        order: &mut Vec<usize>,
        depth: usize,
    ) {
        if done[i] || depth > deps.len() {
            return;
        }
        done[i] = true;
        for &j in &deps[i] {
            visit(j, deps, done, order, depth + 1);
        }
        order.push(i);
    }
    for i in 0..n {
        visit(i, &deps, &mut done, &mut order, 0);
    }
    let mut slots: Vec<Option<(BatchItem, T)>> = items.into_iter().map(Some).collect();
    order.into_iter().filter_map(|i| slots[i].take()).collect()
}

impl Session {
    /// Cheap summary of everything the autosave persists; if it is unchanged
    /// across a call, nothing needs writing.
    pub fn persist_fingerprint(&self) -> (u64, usize, Option<usize>, usize, u32, String) {
        (
            self.model.version,
            self.model.history.nodes.len(),
            self.model.history.current,
            self.model.annotations.len(),
            self.model.next_annotation_id,
            self.model.name.clone(),
        )
    }

    /// Records the just-mutated model in the history tree under `label`. The
    /// tree lives on the model, so it travels with saves and prefab loads.
    /// A copy of the live model without its history tree, for rolling back
    /// a failed edit: cloning the tree copied every snapshot (minutes and
    /// gigabytes on a long session).
    fn model_without_history(&mut self) -> Model {
        let history = std::mem::take(&mut self.model.history);
        let copy = self.model.clone();
        self.model.history = history;
        copy
    }

    /// Puts back a [`Self::model_without_history`] copy, keeping the live
    /// history tree.
    fn restore_model(&mut self, before: Model) {
        let history = std::mem::take(&mut self.model.history);
        self.model = before;
        self.model.history = history;
    }

    fn commit(&mut self, label: &str) {
        if self.batch_depth > 0 {
            self.model.version += 1;
            self.batch_labels.push(label.to_string());
            return;
        }
        self.record(label);
    }

    fn record(&mut self, label: &str) {
        self.model.version += 1;
        // The snapshot carries no tree of its own: the live history on the
        // session model is the single source of truth for the tree shape.
        // Move the tree out first, so the clone doesn't copy every snapshot.
        let mut history = std::mem::take(&mut self.model.history);
        let snapshot = self.model.clone();
        let id = history.record(snapshot, label);
        history.nodes[id].author = self.client.clone();
        self.model.history = history;
    }

    /// Runs `f` as one history step: every commit inside it (including the
    /// earlier items when a later one fails) lands as a single undoable node.
    pub fn batch<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, ModelerError>,
    ) -> Result<T, ModelerError> {
        self.batch_depth += 1;
        let r = f(self);
        self.batch_depth -= 1;
        if self.batch_depth == 0 && !self.batch_labels.is_empty() {
            let labels = std::mem::take(&mut self.batch_labels);
            let label = match labels.as_slice() {
                [one] => one.clone(),
                [first, .., last] => format!("{} edits: {first} … {last}", labels.len()),
                [] => String::new(),
            };
            // The counter already advanced per edit; record() bumps it once more.
            self.model.version -= 1;
            self.record(&label);
        }
        r
    }

    /// Upserts one op into the graph, re-derives the parts and records history.
    /// Re-issuing an op replaces the newest op of the same kind and output name
    /// — never one it reads from, so in-place tweaks (`petal` <- `petal`) are
    /// safe and append instead.
    fn apply_op(&mut self, op: Op, label: &str) -> CmdResult {
        let out = graph::output(&op).to_string();
        let kind = graph::kind(&op);
        let reads = graph::inputs(&op);
        // A visibility edit is in place by nature (reads and writes the same
        // name), yet re-issuing it must replace the previous one, not stack.
        // So is an in-place transform: re-issuing it to adjust the scale or
        // rotation must replace it, or a relative one applies twice.
        let replace_in_place =
            kind == "visibility" || (kind == "transform" && reads.contains(&out.as_str()));
        // "Never replace an op this one reads from" compares the old op's
        // actual outputs: an array named after its source (`tile` -> tile_1..)
        // writes tile_1.., not `tile`, so re-issuing it replaces it.
        let prev = self.model.ops.iter().rposition(|o| {
            graph::output(o) == out
                && graph::kind(o) == kind
                && (replace_in_place
                    || !graph::outputs(o)
                        .iter()
                        .any(|x| reads.contains(&x.as_str())))
        });
        // Reject outputs that collide with another op's outputs (e.g. a
        // primitive `ring_0` vs a radial array `ring`); in-place chains (an op
        // that reads the name it writes) are legitimate and exempt.
        let mine = graph::outputs(&op);
        for (i, other) in self.model.ops.iter().enumerate() {
            if Some(i) == prev {
                continue;
            }
            let other_reads = graph::inputs(other);
            for n in graph::outputs(other) {
                if mine.contains(&n)
                    && !reads.contains(&n.as_str())
                    && !other_reads.contains(&n.as_str())
                {
                    let hint = if graph::kind(&op) == "decimate" {
                        format!(
                            ": a decimate needs its own name: rename the dense source to `{n}_hi` (re-issue it as `{n}_hi`, or a `rename`), then decimate `{n}_hi` into `{n}`"
                        )
                    } else {
                        "; choose a different name".to_string()
                    };
                    return Err(format!(
                        "`{n}` is already produced by another op ({}){hint}",
                        graph::output(other)
                    )
                    .into());
                }
            }
        }
        let old = prev.map(|i| self.model.ops[i].clone());
        match prev {
            Some(i) => self.model.ops[i] = op,
            None => self.model.ops.push(op),
        }
        if let Err(e) = self.model.refresh() {
            // Never leave the graph in a state that does not evaluate.
            match (prev, old) {
                (Some(i), Some(old_op)) => self.model.ops[i] = old_op,
                (None, _) => {
                    self.model.ops.pop();
                }
                (Some(_), None) => {}
            }
            return Err(e);
        }
        self.commit(label);
        Ok(format!("{label} ok"))
    }

    /// A primitive as a graph step (upsert by output name).
    pub fn op_primitive(
        &mut self,
        name: &str,
        kind: PrimitiveKind,
        size: [f32; 3],
        translation: [f32; 3],
        rotation: Option<[f32; 4]>,
    ) -> CmdResult {
        self.apply_op(
            Op::Primitive {
                name: name.into(),
                kind,
                size,
                translation,
                rotation,
                frame: None,
            },
            &format!("op primitive `{name}` ({kind})"),
        )
    }

    /// Transform an earlier part into a new output.
    pub fn op_transform(
        &mut self,
        name: &str,
        input: &str,
        translation: Option<[f32; 3]>,
        rotation: Option<[f32; 4]>,
        scale: Option<[f32; 3]>,
    ) -> CmdResult {
        self.apply_op(
            Op::Transform {
                name: name.into(),
                input: input.into(),
                translation,
                rotation,
                scale,
                relative: false,
                pivot: None,
            },
            &format!("op transform `{name}` <- `{input}`"),
        )
    }

    pub fn op_boolean(&mut self, name: &str, op: BoolOp, a: &str, b: &str) -> CmdResult {
        self.apply_op(
            Op::Boolean {
                name: name.into(),
                op,
                a: a.into(),
                b: b.into(),
            },
            &format!("op boolean `{name}` = {op}({a}, {b})"),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn op_radial_array(
        &mut self,
        name: &str,
        input: &str,
        count: u32,
        center: [f32; 3],
        degrees: f32,
        start_degrees: f32,
        axis: Axis,
        include_source: bool,
    ) -> CmdResult {
        self.apply_op(
            Op::RadialArray {
                name: name.into(),
                input: input.into(),
                count,
                center,
                degrees,
                start_degrees,
                axis,
                include_source,
            },
            &format!("op radial `{name}` = {count} x `{input}` over {degrees}° about {axis}"),
        )
    }

    pub fn op_material(
        &mut self,
        name: &str,
        input: &str,
        base_color: Option<[f32; 4]>,
        roughness: Option<f32>,
        metalness: Option<f32>,
    ) -> CmdResult {
        self.apply_op(
            Op::Material {
                name: name.into(),
                input: input.into(),
                base_color,
                roughness,
                metalness,
                pbr: None,
                flat: false,
            },
            &format!("op material `{name}` <- `{input}`"),
        )
    }

    /// Copies an earlier part in a straight line.
    pub fn op_linear_array(
        &mut self,
        name: &str,
        input: &str,
        count: u32,
        offset: [f32; 3],
    ) -> CmdResult {
        self.apply_op(
            Op::LinearArray {
                name: name.into(),
                input: input.into(),
                count,
                offset,
                local: false,
            },
            &format!("op linear `{name}` = {count} x `{input}`"),
        )
    }

    /// Mirrors an earlier part across a plane (axis x/y/z at offset).
    pub fn op_mirror(
        &mut self,
        name: &str,
        input: &str,
        axis: Axis,
        offset: [f32; 3],
    ) -> CmdResult {
        self.apply_op(
            Op::Mirror {
                name: name.into(),
                input: input.into(),
                axis,
                offset,
            },
            &format!("op mirror `{name}` = `{input}` across {axis}"),
        )
    }

    /// Removes the op whose output is `name` and re-derives.
    pub fn op_delete(&mut self, name: &str) -> CmdResult {
        let Some(idx) = self.model.ops.iter().position(|o| graph::output(o) == name) else {
            return Err(format!("no op with output `{name}`").into());
        };
        let produced = graph::outputs(&self.model.ops[idx]);
        let dependents: Vec<String> = self
            .model
            .ops
            .iter()
            .enumerate()
            .filter(|&(i, o)| {
                // In-place edits of the deleted part (hide, material,
                // transform on itself) go with it.
                let own_edit = graph::inputs(o) == [graph::output(o).to_string()]
                    && produced.iter().any(|p| p == graph::output(o));
                i != idx
                    && !own_edit
                    && graph::inputs(o)
                        .iter()
                        .any(|inp| produced.iter().any(|p| p == inp))
            })
            .map(|(_, o)| format!("`{}`", graph::output(o)))
            .collect();
        if !dependents.is_empty() {
            return Err(format!(
                "can't delete `{name}`: still read by {}; delete or repoint those first",
                dependents.join(", ")
            )
            .into());
        }
        self.model.ops.retain(|o| {
            let own_edit = graph::inputs(o) == [graph::output(o).to_string()]
                && produced.iter().any(|p| p == graph::output(o));
            graph::output(o) != name && !own_edit
        });
        let live: Vec<String> = self.model.ops.iter().map(Self::template_key).collect();
        self.model.templates.retain(|k, _| live.contains(k));
        self.model.refresh()?;
        self.commit(&format!("op delete `{name}`"));
        Ok(format!("deleted op `{name}`"))
    }

    /// The op graph, one line per step.
    pub fn describe_ops(&self) -> String {
        graph::describe(&self.model.ops)
    }

    /// The describe line for a single op, used as its history label.
    fn op_label(op: &Op) -> String {
        graph::describe(std::slice::from_ref(op))
            .lines()
            .nth(1)
            .map(|l| l.replacen("#0 ", "", 1).to_string())
            .unwrap_or_else(|| "op".into())
    }

    /// Replaces the session with a fresh empty model. The old model is recorded
    /// in the (carried-over) history tree first, so it is one `undo` away —
    /// reset without losing work.
    pub fn new_model(&mut self, name: &str) -> CmdResult {
        // Another agent's document, or one from another project: keep it
        // open and start the new one beside it instead of replacing it.
        let theirs = self
            .clients
            .iter()
            .any(|(c, v)| self.client.as_ref() != Some(c) && v.doc == self.model.name);
        let elsewhere = crate::catalog::call_project().is_some_and(|p| p != self.model.project)
            && !(self.model.ops.is_empty() && self.model.parts.is_empty());
        if theirs || elsewhere {
            let old = self.model.name.clone();
            let mut model = Model::named(name);
            model.version = self.model.version + 1;
            model.project = crate::catalog::project_root();
            let outgoing = std::mem::replace(&mut self.model, model);
            crate::catalog::set_open_doc(&outgoing.name, Some(outgoing.clone_without_history()));
            self.others.push(outgoing);
            self.pending_pin = None;
            return Ok(format!(
                "started new model `{name}` as its own document (`{old}` stays open: {})",
                if theirs {
                    "another agent is working in it"
                } else {
                    "it belongs to another project"
                }
            ));
        }
        let history = std::mem::take(&mut self.model.history);
        let snapshot = self.model.clone();
        let mut model = Model::named(name);
        model.history = history;
        model.version = self.model.version + 1;
        model
            .history
            .record(snapshot, format!("new model `{name}`"));
        self.pending_pin = None;
        self.model = model;
        Ok(format!(
            "started new model `{name}` (the old model is one undo away)"
        ))
    }

    /// Applies a batch of ops in order, one graph application each (same
    /// upsert/rollback semantics as the individual op tools). Stops at the
    /// first error with its index; the ops before it stay applied.
    pub fn apply_ops(&mut self, ops: Vec<Op>) -> CmdResult {
        self.apply_batch(ops.into_iter().map(BatchItem::Op).collect())
    }

    /// The `apply_ops` entry point: raw JSON items plus optional parameter
    /// updates. Parameters are merged first; every item is resolved against
    /// them and parsed before anything is applied (a malformed batch changes
    /// nothing). Ops whose items reference parameters keep their raw item as a
    /// template, so a later parameter change re-derives them.
    pub fn apply_values(
        &mut self,
        params: Option<crate::params::Params>,
        items: Vec<serde_json::Value>,
    ) -> CmdResult {
        self.apply_values_as(params, items, true)
    }

    /// [`Self::apply_values`] ending in the full scene summary (`full`) or a
    /// brief one: counts, bounds, which parts were added / changed / removed,
    /// and the warnings. The brief form keeps a long build's context small.
    pub fn apply_values_as(
        &mut self,
        params: Option<crate::params::Params>,
        items: Vec<serde_json::Value>,
        full: bool,
    ) -> CmdResult {
        let before = self.part_fingerprints();
        // Atomic: a failing item rolls the whole call back.
        // The model without its history (the history only appends, so a
        // mark rolls it back): cloning the whole tree here was most of a
        // small edit's cost on a long session.
        let history = std::mem::take(&mut self.model.history);
        let mark = history.mark();
        let snapshot = self.model.clone();
        self.model.history = history;
        let text = match self.batch(|s| s.apply_values_inner(params, items)) {
            Ok(t) => t,
            Err(e) => {
                let version = self.model.version + 1;
                let mut history = std::mem::take(&mut self.model.history);
                history.rollback(&mark);
                self.model = snapshot;
                self.model.history = history;
                self.model.version = version;
                let msg = e.to_string();
                return Err(if msg.contains("nothing was applied") {
                    msg
                } else {
                    format!("{msg}\n(rolled back: nothing from this call was applied)")
                }
                .into());
            }
        };
        if full {
            return Ok(format!("{text}\n{}", self.summary()));
        }
        // Brief: routine per-item echoes collapse to a count; anything else
        // (param lines, notes) stays.
        let routine = |l: &str| {
            l.starts_with('#')
                && (l.ends_with(" ok") || l.ends_with(" hidden") || l.ends_with(" shown"))
        };
        let n = text.lines().filter(|l| l.starts_with('#')).count();
        let mut kept: Vec<&str> = text.lines().filter(|l| !routine(l)).collect();
        let applied = format!("applied {n} item(s)");
        if n > 0 || kept.is_empty() {
            kept.insert(0, &applied);
        }
        Ok(format!(
            "{}\n{}",
            kept.join("\n"),
            self.brief_summary(&before)
        ))
    }

    /// A content hash per part (geometry, transform, material, visibility),
    /// to tell which parts a call touched.
    pub fn part_fingerprints(&self) -> std::collections::BTreeMap<String, u64> {
        use std::hash::{Hash, Hasher};
        self.model
            .parts
            .iter()
            .map(|p| {
                let mut h = std::collections::hash_map::DefaultHasher::new();
                for v in &p.verts {
                    v.iter().for_each(|f| f.to_bits().hash(&mut h));
                }
                p.indices.hash(&mut h);
                for f in p
                    .translation
                    .iter()
                    .chain(&p.rotation)
                    .chain(&p.scale)
                    .chain(&p.material.base_color)
                {
                    f.to_bits().hash(&mut h);
                }
                p.material.roughness.to_bits().hash(&mut h);
                p.material.metalness.to_bits().hash(&mut h);
                p.visible.hash(&mut h);
                (p.name.clone(), h.finish())
            })
            .collect()
    }

    /// The short result tail for `apply_ops`: model line, the parts this call
    /// added (+), changed (~) and removed (-), and every warning.
    pub fn brief_summary(&self, before: &std::collections::BTreeMap<String, u64>) -> String {
        let after = self.part_fingerprints();
        let bounds = self
            .model
            .bounds()
            .map(|(min, max)| format!("bounds {min:?}..{max:?}"))
            .unwrap_or_else(|| "empty".into());
        let mut s = format!(
            "model `{}` v{}: {} parts, {} ops, {bounds}\n",
            self.model.name,
            self.model.version,
            self.model.parts.len(),
            self.model.ops.len()
        );
        // Model order (not alphabetical), so array copies stay adjacent and
        // fold into `name_1..N`.
        let (mut added, mut changed, mut removed) = (Vec::new(), Vec::new(), Vec::new());
        for p in &self.model.parts {
            let n = &p.name;
            match (before.get(n), after.get(n)) {
                (None, _) => added.push(n.clone()),
                (Some(b), Some(h)) if b != h => changed.push(n.clone()),
                _ => {}
            }
        }
        for n in before.keys() {
            if !after.contains_key(n) {
                removed.push(n.clone());
            }
        }
        let count = added.len() + changed.len() + removed.len();
        let mut changes = Vec::new();
        for (sign, list) in [("+", &added), ("~", &changed), ("-", &removed)] {
            changes.extend(
                crate::compact_names(list)
                    .into_iter()
                    .map(|n| format!("{sign}{n}")),
            );
        }
        if changes.is_empty() {
            s += "parts: no change\n";
        } else {
            let shown = changes.len().min(40);
            s += &format!(
                "parts ({count} changed): {}{}\n",
                changes[..shown].join(", "),
                if changes.len() > shown { ", ..." } else { "" }
            );
        }
        if self.pending_pin.is_some() {
            s += "a pin is waiting in the live view: use add_annotation without face/vertex/position to comment on it\n";
        }
        let lints = self.lints();
        if lints.is_empty() {
            s += "warnings: none\n";
        } else {
            s += &format!("warnings ({}):\n", lints.len());
            for l in &lints {
                s += &format!("- {l}\n");
            }
        }
        s += "(brief result: scene_summary lists every part; pass summary: \"full\" to get it here)\n";
        s
    }

    /// Embeds the graph of a `material` item's `pbr` (from the working
    /// materials first, then the library), so the model is self-contained.
    fn resolve_pbr(&self, item: &mut BatchItem) -> Result<(), String> {
        let r = match item {
            BatchItem::Op(Op::Material { pbr: Some(r), .. }) => r,
            BatchItem::Op(Op::Deform { def, .. }) if def.pbr.is_some() => match &mut def.pbr {
                Some(r) => r,
                None => return Ok(()),
            },
            _ => return Ok(()),
        };
        let doc = match self.materials.docs.get(&r.material) {
            Some(d) => d.clone(),
            None => crate::catalog::load_doc::<crate::material::MatDoc>(
                crate::catalog::Kind::Materials,
                &r.material,
            )
            .map_err(|_| {
                let working: Vec<&str> = self.materials.docs.keys().map(String::as_str).collect();
                format!(
                    "pbr: no material `{}` (working: {}; saved: material_catalog list)",
                    r.material,
                    if working.is_empty() {
                        "none".into()
                    } else {
                        working.join(", ")
                    }
                )
            })?,
        };
        if let Some(k) = r.params.keys().find(|k| !doc.params.contains_key(*k)) {
            let known: Vec<&str> = doc.params.keys().map(String::as_str).collect();
            return Err(format!(
                "pbr `{}`: no param `{k}` (params: {})",
                r.material,
                if known.is_empty() {
                    "none".into()
                } else {
                    known.join(", ")
                }
            ));
        }
        r.graph = Some(std::sync::Arc::new(doc.graph()));
        Ok(())
    }

    /// After `material_ops` edits `material`, re-embed its graph in every
    /// material item of the active model that uses it, so parts follow the
    /// edit without re-issuing those items. One history step; returns how
    /// many items were refreshed.
    pub fn refresh_pbr_users(&mut self, material: &str) -> Result<usize, String> {
        let Some(doc) = self.materials.docs.get(material) else {
            return Ok(0);
        };
        let graph = std::sync::Arc::new(doc.graph());
        let before = self.model_without_history();
        let mut n = 0;
        for op in &mut self.model.ops {
            let r = match op {
                Op::Material { pbr: Some(r), .. } => Some(r),
                Op::Deform { def, .. } => def.pbr.as_mut(),
                _ => None,
            };
            if let Some(r) = r
                && r.material == material
            {
                r.graph = Some(graph.clone());
                n += 1;
            }
        }
        if n == 0 {
            return Ok(0);
        }
        if let Err(e) = self.model.refresh() {
            self.restore_model(before);
            return Err(format!("re-applying `{material}` to the model failed: {e}"));
        }
        self.commit(&format!("material {material} edited"));
        self.mark_autosave(true, false, false);
        Ok(n)
    }

    /// `material` items whose `name` has a `*` (and no `input`) become one
    /// in-place item per matching part: the model's parts plus every name
    /// this batch makes. Copies of a matched source inherit it anyway.
    fn expand_material_globs(
        &self,
        items: Vec<(serde_json::Value, Option<String>)>,
        style: &crate::style::Style,
        full: &crate::params::Params,
    ) -> Result<Vec<(serde_json::Value, Option<String>)>, String> {
        let mut names: Vec<String> = self.model.parts.iter().map(|p| p.name.clone()).collect();
        for (raw, _) in &items {
            // Moves, places, colliders, lights and markers are named too,
            // but make no part: `"bottle_*"` must not match `bottle_on`.
            let makes_part = !matches!(
                raw.get("type").and_then(|v| v.as_str()),
                Some("move" | "place" | "collider" | "light" | "marker")
            );
            if let Some(n) = raw.get("name").and_then(|v| v.as_str())
                && makes_part
                && !n.contains('*')
            {
                names.push(n.to_string());
                // Every part the op declares, when the item already parses
                // (a path_array by spacing, a split's halves). A scatter
                // declares every slot it might fill (up to `max`), so it
                // keeps the literal-count guess below.
                let ty0 = raw.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if ty0 != "scatter"
                    && let Ok(v) = crate::style::expand(raw, style)
                    && let Ok(v) = crate::params::resolve(&v, full)
                    && let Ok(BatchItem::Op(op)) = crate::spec::parse_item(0, &v)
                {
                    names.extend(crate::graph::outputs(&op));
                }
                // Parts an item writes besides its name (before params are
                // read, so only literal counts): lsystem leaves, array copies.
                let ty = raw.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if ty == "lsystem" {
                    let text = |k: &str| raw.get(k).map(|v| v.to_string()).unwrap_or_default();
                    // Whatever the item leaves out comes from its preset.
                    let base = raw
                        .get("preset")
                        .and_then(|v| v.as_str())
                        .and_then(|p| crate::spec::lsystem_preset(p).ok());
                    let preset_axiom = base.as_ref().is_some_and(|b| b.axiom.contains('L'));
                    let preset_rules = base.as_ref().is_some_and(|b| {
                        b.rules
                            .iter()
                            .flat_map(|r| &r.1)
                            .any(|c| c.to.contains('L'))
                    });
                    let leafy = match raw.get("axiom") {
                        Some(_) => text("axiom").contains('L'),
                        None => preset_axiom,
                    } || match raw.get("rules") {
                        Some(_) => text("rules").contains('L'),
                        None => preset_rules,
                    };
                    if leafy {
                        names.push(format!("{n}_leaves"));
                    }
                }
                if ty == "split" {
                    let keep = raw.get("keep").and_then(|v| v.as_str()).unwrap_or("both");
                    for side in ["above", "below"] {
                        if keep == "both" || keep == side {
                            names.push(format!("{n}_{side}"));
                        }
                    }
                }
                if let Some(c) = raw.get("count").and_then(|v| v.as_u64()) {
                    match ty {
                        "linear_array" => names.extend((1..c).map(|i| format!("{n}_{i}"))),
                        "path_array" => names.extend((0..c).map(|i| format!("{n}_{i}"))),
                        "scatter" => {
                            // Each copy of a set gets its members' suffixes
                            // (`trees_0`, `trees_0_leaves`); globs resolve later.
                            let sets: Vec<Vec<String>> = match raw.get("input") {
                                Some(serde_json::Value::String(x)) => vec![vec![x.clone()]],
                                Some(serde_json::Value::Array(a)) => a
                                    .iter()
                                    .map(|v| match v {
                                        serde_json::Value::Array(m) => m
                                            .iter()
                                            .filter_map(|x| x.as_str().map(String::from))
                                            .collect(),
                                        v => v.as_str().map(String::from).into_iter().collect(),
                                    })
                                    .filter(|s: &Vec<String>| !s.is_empty())
                                    .collect(),
                                _ => Vec::new(),
                            };
                            let mut sfx: Vec<String> = Vec::new();
                            for set in &sets {
                                for x in crate::scatter::member_suffixes(set) {
                                    if !sfx.contains(&x) {
                                        sfx.push(x);
                                    }
                                }
                            }
                            if sfx.is_empty() {
                                sfx.push(String::new());
                            }
                            for i in 0..c {
                                names.extend(sfx.iter().map(|x| format!("{n}_{i}{x}")));
                            }
                        }
                        "radial_array" => {
                            let first =
                                raw.get("include_source").and_then(|v| v.as_bool()) != Some(false);
                            names.extend((u64::from(first)..c).map(|i| format!("{n}_{i}")));
                        }
                        _ => {}
                    }
                }
            }
        }
        names.sort();
        names.dedup();
        let mut out = Vec::with_capacity(items.len());
        for (raw, from) in items {
            let pat = raw.get("name").and_then(|v| v.as_str()).map(str::to_string);
            let is_glob = raw.get("type").and_then(|v| v.as_str()) == Some("material")
                && raw.get("input").is_none()
                && pat.as_deref().is_some_and(|p| p.contains('*'));
            let (true, Some(pat)) = (is_glob, pat) else {
                out.push((raw, from));
                continue;
            };
            let hits: Vec<&String> = names
                .iter()
                .filter(|n| crate::name_matches(&pat, n))
                .collect();
            if hits.is_empty() {
                return Err(format!("material `{pat}`: the pattern matches no part"));
            }
            for n in hits {
                let mut v = raw.clone();
                v["name"] = serde_json::Value::String(n.clone());
                out.push((v, from.clone()));
            }
        }
        Ok(out)
    }

    fn apply_values_inner(
        &mut self,
        params: Option<crate::params::Params>,
        items: Vec<serde_json::Value>,
    ) -> CmdResult {
        // Drop templates whose op is gone (deleted, or failed to apply).
        let live: Vec<String> = self.model.ops.iter().map(Self::template_key).collect();
        self.model.templates.retain(|k, _| live.contains(k));
        let mut merged = self.model.params.clone();
        let mut changed: Vec<String> = Vec::new();
        for (k, v) in params.unwrap_or_default() {
            if merged.get(&k) != Some(&v) {
                changed.push(k.clone());
            }
            merged.insert(k, v);
        }
        // Loops first: every repeat becomes plain items (its variable
        // substituted), so each copy gets its own template below.
        // Parse everything against the merged parameters (over the style's
        // number tokens) with the style's tokens expanded.
        let style = self.model.style.clone();
        let full = style.params_under(&merged);
        let items = crate::spec::expand_repeats(&items, &full)
            .map_err(|e| format!("{e}\n(nothing was applied)"))?;
        let items = self
            .expand_material_globs(items, &style, &full)
            .map_err(|e| format!("{e}\n(nothing was applied)"))?;
        let mut parsed = Vec::with_capacity(items.len());
        for (i, (raw, from)) in items.iter().enumerate() {
            let from = from
                .as_ref()
                .map_or_else(String::new, |n| format!(" [from {n}]"));
            let resolved = crate::style::expand(raw, &style)
                .and_then(|v| crate::params::resolve(&v, &full))
                .map_err(|e| format!("op #{i}{from}: {e}\n(nothing was applied)"))?;
            let mut item = crate::spec::parse_item(i, &resolved)
                .map_err(|e| format!("{e}{from}\n(nothing was applied)"))?;
            self.resolve_pbr(&mut item)
                .map_err(|e| format!("op #{i}{from}: {e}\n(nothing was applied)"))?;
            if let BatchItem::Op(op) = &mut item {
                self.resolve_prefab(op)
                    .map_err(|e| format!("op #{i}{from}: {e}\n(nothing was applied)"))?;
            }
            let template = (!crate::params::refs(raw).is_empty()
                || crate::style::reads_style(raw, &style))
            .then(|| raw.clone());
            parsed.push((item, template));
        }
        // Templated ops already in the graph that read a changed parameter
        // (and are not re-issued by this batch) are re-derived first.
        let reissued: Vec<String> = parsed
            .iter()
            .filter_map(|(it, _)| match it {
                BatchItem::Op(op) => Some(Self::template_key(op)),
                _ => None,
            })
            .collect();
        let mut stale = Vec::new();
        for (name, raw) in &self.model.templates {
            if reissued.contains(name) {
                continue;
            }
            if crate::params::refs(raw).iter().any(|r| changed.contains(r)) {
                let resolved = crate::style::expand(raw, &style)
                    .and_then(|v| crate::params::resolve(&v, &full))
                    .map_err(|e| format!("re-deriving `{name}`: {e}"))?;
                let mut item = crate::spec::parse_item(0, &resolved)
                    .map_err(|e| format!("re-deriving `{name}`: {e}"))?;
                self.resolve_pbr(&mut item)
                    .map_err(|e| format!("re-deriving `{name}`: {e}"))?;
                if let BatchItem::Op(op) = &mut item {
                    self.resolve_scatter(op)
                        .map_err(|e| format!("re-deriving `{name}`: {e}"))?;
                }
                stale.push((item, Some(raw.clone())));
            }
        }
        let mut lines = Vec::new();
        if !changed.is_empty() {
            // One history step for the whole change: swap the dependent ops in
            // place, re-derive once, commit once (undo restores both).
            let before = self.model_without_history();
            self.model.params = merged;
            let n = stale.len();
            for (item, _) in stale {
                if let BatchItem::Op(op) = item
                    && let Some(slot) = self.model.ops.iter_mut().find(|o| {
                        graph::output(o) == graph::output(&op) && graph::kind(o) == graph::kind(&op)
                    })
                {
                    *slot = op;
                }
            }
            if let Err(e) = self.model.refresh() {
                self.restore_model(before);
                return Err(format!("re-deriving ops for changed params failed: {e}").into());
            }
            self.commit(&format!("params {}", changed.join(", ")));
            lines.push(format!(
                "params: {}",
                self.model
                    .params
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            if n > 0 {
                lines.push(format!("re-derived {n} op(s) that read changed params"));
            }
        }
        // Re-issuing an op another client wrote replaces their work: say so.
        let doc = self.model.name.clone();
        let keys: Vec<String> = parsed
            .iter()
            .filter_map(|(it, _)| match it {
                BatchItem::Op(op) => Some(Self::template_key(op)),
                _ => None,
            })
            .collect();
        let mut taken = Vec::new();
        if let Some(me) = self.client.clone() {
            let live: Vec<String> = self.model.ops.iter().map(Self::template_key).collect();
            for k in &keys {
                if let Some(other) = self.op_authors.get(&(doc.clone(), k.clone()))
                    && *other != me
                    && live.contains(k)
                {
                    taken.push(format!("`{k}` (by {other})"));
                }
            }
        }
        lines.push(self.apply_items(parsed, false)?);
        if let Some(me) = self.client.clone() {
            for k in keys {
                self.op_authors.insert((doc.clone(), k), me.clone());
            }
        }
        if !taken.is_empty() {
            taken.dedup();
            lines.push(format!(
                "WARNING this call replaced op(s) another agent wrote in `{doc}`: {} (diff / history show their version; undo reverts this call)",
                taken.join(", ")
            ));
        }
        Ok(lines.join("\n"))
    }

    /// Replaces the model's style (or patches it with `patch`) and re-derives
    /// every templated op whose resolved form changed, as one history step.
    /// Resolves a scatter's input entries against the live model: a `*`
    /// pattern becomes the set of visible parts it matches (a prefab
    /// instance), and a lone lsystem name brings its leaves along.
    pub fn resolve_scatter(&self, op: &mut Op) -> Result<(), ModelerError> {
        let Op::Scatter {
            name, input, on, ..
        } = op
        else {
            return Ok(());
        };
        let mine = format!("{name}_");
        let mut out = Vec::with_capacity(input.len());
        for set in input.iter() {
            let mut resolved: Vec<String> = Vec::new();
            for entry in set {
                if entry.contains('*') {
                    let hits: Vec<String> = self
                        .model
                        .parts
                        .iter()
                        .filter(|p| {
                            p.visible
                                && crate::name_matches(entry, &p.name)
                                && !p.name.starts_with(&mine)
                                && p.name != *on
                        })
                        .map(|p| p.name.clone())
                        .collect();
                    if hits.is_empty() {
                        return Err(
                            format!("scatter `{name}`: `{entry}` matches no visible part").into(),
                        );
                    }
                    resolved.extend(hits);
                } else {
                    resolved.push(entry.clone());
                }
            }
            if set.len() == 1 && !set[0].contains('*') {
                let leafy = self.model.ops.iter().any(|o| {
                    matches!(o, Op::LSystem { name: n, def } if *n == set[0] && def.has_leaves())
                });
                let leaves = format!("{}_leaves", set[0]);
                if leafy && self.model.parts.iter().any(|p| p.name == leaves) {
                    resolved.push(leaves);
                }
            }
            resolved.dedup();
            out.push(resolved);
        }
        *input = out;
        Ok(())
    }

    /// For a scatter with an exact count: its name and that count.
    fn scatter_note_for(&self, op: &Op) -> Option<(String, Option<u32>)> {
        match op {
            Op::Scatter { name, def, .. } => Some((name.clone(), def.count)),
            _ => None,
        }
    }

    /// The scatter result line: how many copies landed (and why fewer).
    fn scatter_note(&self, name: &str, want: Option<u32>) -> String {
        let have: std::collections::HashSet<&str> =
            self.model.parts.iter().map(|p| p.name.as_str()).collect();
        let mut n = 0;
        while have.contains(format!("{name}_{n}").as_str()) {
            n += 1;
        }
        let range = match n {
            0 => "none".to_string(),
            1 => format!("{name}_0"),
            _ => format!("{name}_0..{}", n - 1),
        };
        match want {
            Some(w) if n < w as usize => format!(
                "scatter `{name}`: placed {n} of {w} ({range}); min_spacing, max_slope or region left no room for more (lower min_spacing or widen the surface)"
            ),
            _ => format!("scatter `{name}`: placed {n} ({range}); hide the source part(s)"),
        }
    }

    /// Refreshes a prefab op from its source (an open document first, else
    /// the catalog file): the visible part list, the digest, and the style it
    /// is re-derived with. Returns whether anything changed.
    pub fn resolve_prefab(&self, op: &mut Op) -> Result<bool, ModelerError> {
        let Op::Prefab {
            prefab,
            parts,
            digest,
            style,
            restyle,
            ..
        } = op
        else {
            return Ok(false);
        };
        let src = crate::catalog::prefab_instance(prefab)?;
        let new_restyle = match style {
            None => None,
            Some(mode) => {
                let mut st = src.style.clone();
                let patch = if mode.as_str() == Some("scene") {
                    serde_json::to_value(&self.model.style).map_err(|e| e.to_string())?
                } else {
                    mode.clone()
                };
                st.merge(&patch)
                    .map_err(|e| format!("prefab `{prefab}` style: {e}"))?;
                Some(st)
            }
        };
        let new_parts: Vec<String> = src
            .parts
            .iter()
            .filter(|p| p.visible)
            .map(|p| p.name.clone())
            .collect();
        if new_parts.is_empty() {
            return Err(format!("prefab `{prefab}` has no visible parts").into());
        }
        let new_digest = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            serde_json::to_string(&src.ops)
                .unwrap_or_default()
                .hash(&mut h);
            serde_json::to_string(&src.style)
                .unwrap_or_default()
                .hash(&mut h);
            new_parts.hash(&mut h);
            h.finish()
        };
        let changed = *parts != new_parts || *digest != new_digest || *restyle != new_restyle;
        *parts = new_parts;
        *digest = new_digest;
        *restyle = new_restyle;
        Ok(changed)
    }

    /// Re-resolves every prefab instance in the active model (after its
    /// source document changed or the scene style moved) and re-derives.
    /// Returns the instances that changed; records one history step if any.
    pub fn sync_prefabs(&mut self) -> Result<Vec<String>, ModelerError> {
        let mut ops = self.model.ops.clone();
        let mut changed = Vec::new();
        for op in &mut ops {
            if self.resolve_prefab(op)? {
                changed.push(graph::output(op).to_string());
            }
        }
        if changed.is_empty() {
            return Ok(changed);
        }
        let before = self.model_without_history();
        self.model.ops = ops;
        if let Err(e) = self.model.refresh() {
            self.restore_model(before);
            return Err(format!("re-deriving prefab instances failed: {e}").into());
        }
        self.commit("sync prefabs");
        Ok(changed)
    }

    /// A document restored from the autosave holds only its ops (parts
    /// re-derive): derive them when it becomes the active one, or it shows
    /// nothing but its marker gizmos.
    fn derive_if_bare(&mut self) {
        if !self.model.parts.is_empty() || self.model.ops.is_empty() {
            return;
        }
        let p = self.model.project.clone();
        let r = if crate::catalog::call_project().is_none() {
            crate::catalog::with_project(p, || self.model.refresh())
        } else {
            self.model.refresh()
        };
        if let Err(e) = r {
            eprintln!("deriving `{}`: {e}", self.model.name);
        }
        // Panes that drew the bare document redraw.
        self.model.version += 1;
    }

    /// Open documents: `list`, `open` (an open doc, the catalog entry, or a
    /// new empty model), `switch` (same as open for an open doc), `close`.
    /// The active document is `self.model`; the rest wait in `others`, each
    /// with its own history, and are visible to prefab instances by name.
    pub fn scene(&mut self, action: &str, name: Option<&str>) -> CmdResult {
        self.scene_impl(action, name)
    }

    /// `scene` from the human's window: the document it switches to, opens
    /// or restores becomes the one the window stays on. While agent calls
    /// are in flight their document stays active (the window catches up
    /// when they end), and the document an agent is working in can't be
    /// closed under it.
    pub fn window_scene(&mut self, action: &str, name: Option<&str>) -> CmdResult {
        let busy = self.in_flight > 0;
        let prev = self.model.name.clone();
        if busy && action == "close" && name.is_none_or(|n| n == prev) {
            return Err(
                format!("an agent is working in `{prev}` right now; close it in a moment").into(),
            );
        }
        if busy && action == "switch" {
            let n = name.ok_or("scene switch: needs a name")?;
            if n != prev && !self.others.iter().any(|m| m.name == n) {
                return Err(format!("no open document `{n}`").into());
            }
            self.viewer_doc = Some(n.to_string());
            return Ok(format!("showing `{n}`"));
        }
        let r = self.scene_impl(action, name)?;
        match action {
            "switch" | "open" | "restore" => self.viewer_doc = Some(self.model.name.clone()),
            "close" if self.viewer_doc.as_deref() == Some(name.unwrap_or(prev.as_str())) => {
                self.viewer_doc = None;
            }
            _ => {}
        }
        // Give the agent its document back until its calls end.
        if busy && self.model.name != prev && self.others.iter().any(|m| m.name == prev) {
            let _ = self.scene_impl("switch", Some(&prev));
        }
        Ok(r)
    }

    /// True while agent calls are in flight on another document than the
    /// one the human is looking at: the window keeps showing what it showed
    /// and holds anything it would write.
    pub fn viewer_away(&self) -> bool {
        self.in_flight > 0
            && self
                .viewer_doc
                .as_ref()
                .is_some_and(|v| *v != self.model.name)
    }

    /// The active document was renamed (set_model_name): a window on it
    /// follows the new name.
    pub fn renamed_active(&mut self, old: &str) {
        if self.viewer_doc.as_deref() == Some(old) {
            self.viewer_doc = Some(self.model.name.clone());
        }
    }

    fn scene_impl(&mut self, action: &str, name: Option<&str>) -> CmdResult {
        let need = || name.ok_or_else(|| format!("scene {action}: needs a name"));
        match action {
            "list" => {
                let mut s = String::from("open documents:\n");
                let line = |m: &Model, active: bool| {
                    let uses: Vec<&str> = m
                        .ops
                        .iter()
                        .filter_map(|o| match o {
                            Op::Prefab { prefab, .. } => Some(prefab.as_str()),
                            _ => None,
                        })
                        .collect::<std::collections::BTreeSet<_>>()
                        .into_iter()
                        .collect();
                    format!(
                        "{} {} ({} parts, {} ops{}{})\n",
                        if active { "*" } else { " " },
                        m.name,
                        m.parts.iter().filter(|p| p.visible).count(),
                        m.ops.len(),
                        if m.style.brief.is_empty() {
                            ""
                        } else {
                            ", styled"
                        },
                        if uses.is_empty() {
                            String::new()
                        } else {
                            format!(", instances {}", uses.join(", "))
                        }
                    )
                };
                s += &line(&self.model, true);
                for m in &self.others {
                    s += &line(m, false);
                }
                if !self.parked.is_empty() {
                    s += &self.parked_note();
                }
                Ok(s)
            }
            "open" | "switch" => {
                let name = need()?;
                if name == self.model.name {
                    return Ok(format!("`{name}` is already the active document"));
                }
                if let Some(want) = crate::catalog::call_project()
                    && let Some(other) = self.others.iter().find(|m| m.name == name)
                    && other.project != want
                {
                    return Err(format!(
                        "a document named `{name}` from {} is open; this call works in {}. Close it (scene close) or pick another name",
                        project_label(other.project.as_deref()),
                        project_label(want.as_deref())
                    )
                    .into());
                }
                let (incoming, how) =
                    if let Some(i) = self.others.iter().position(|m| m.name == name) {
                        (self.others.remove(i), "switched to")
                    } else if action == "switch" {
                        return Err(format!(
                            "no open document `{name}` (open: {}); use action open",
                            self.doc_names().join(", ")
                        )
                        .into());
                    } else {
                        match crate::catalog::load_prefab_file(name) {
                            Ok(mut m) => {
                                m.name = name.to_string();
                                m.project = crate::catalog::project_root();
                                if let Some(h) = crate::catalog::prefab_file_hash(name) {
                                    self.catalog_base.insert(crate::catalog::sanitize(name), h);
                                }
                                self.saved_state.insert(
                                    name.to_string(),
                                    (m.history.nodes.len(), m.history.current),
                                );
                                (m, "opened catalog entry")
                            }
                            Err(_) => {
                                let mut m = Model::named(name);
                                m.project = crate::catalog::project_root();
                                (m, "opened new empty document")
                            }
                        }
                    };
                let bump = self.model.version.max(incoming.version) + 1;
                let outgoing = std::mem::replace(&mut self.model, incoming);
                crate::catalog::set_open_doc(
                    &outgoing.name,
                    Some(outgoing.clone_without_history()),
                );
                crate::catalog::set_open_doc(name, None);
                self.others.push(outgoing);
                self.model.version = bump;
                self.pending_pin = None;
                self.derive_if_bare();
                // The window switching documents (no call): look prefabs up
                // in the incoming document's own project.
                let synced = if crate::catalog::call_project().is_none() {
                    let p = self.model.project.clone();
                    crate::catalog::with_project(p, || self.sync_prefabs())?
                } else {
                    self.sync_prefabs()?
                };
                let mut s = format!("{how} `{name}` (open: {})", self.doc_names().join(", "));
                if !synced.is_empty() {
                    s += &format!(
                        "\nupdated prefab instances from their sources: {}",
                        synced.join(", ")
                    );
                }
                Ok(s)
            }
            "close" => {
                let name = need()?;
                if name == self.model.name {
                    let Some(next) = self.others.pop() else {
                        return Err("can't close the only open document".into());
                    };
                    crate::catalog::set_open_doc(&next.name, None);
                    let bump = self.model.version.max(next.version) + 1;
                    self.model = next;
                    self.model.version = bump;
                    self.derive_if_bare();
                } else {
                    let Some(i) = self.others.iter().position(|m| m.name == name) else {
                        if self.parked.iter().any(|m| m.name == name) {
                            let i = self.parked_index(name)?;
                            self.parked.remove(i);
                            self.save_parked();
                            return Ok(format!(
                                "dropped the parked autosave of `{name}` (it was not open)"
                            ));
                        }
                        return Err(format!("no open document `{name}`").into());
                    };
                    self.others.remove(i);
                }
                crate::catalog::set_open_doc(name, None);
                Ok(format!(
                    "closed `{name}` (its autosave is dropped; save_prefab first to keep it). open: {}",
                    self.doc_names().join(", ")
                ))
            }
            "restore" => {
                let Some(name) = name else {
                    return Ok(if self.parked.is_empty() {
                        "no parked documents: nothing unsaved was left in the autosave".into()
                    } else {
                        self.parked_note()
                    });
                };
                let i = self.parked_index(name)?;
                if self.model.name == name || self.others.iter().any(|m| m.name == name) {
                    return Err(format!(
                        "`{name}` is open already (from the catalog or new); close it first (scene close) to restore the autosaved copy"
                    )
                    .into());
                }
                let m = self.parked.remove(i);
                let v = m.version;
                self.others.push(m);
                match self.scene("switch", Some(name)) {
                    Ok(r) => {
                        self.save_parked();
                        Ok(format!("restored the autosaved `{name}` (v{v}); {r}"))
                    }
                    Err(e) => {
                        // Back to parked: nothing half-restored.
                        if let Some(k) = self.others.iter().rposition(|m| m.name == name) {
                            let m = self.others.remove(k);
                            self.parked.insert(i.min(self.parked.len()), m);
                        }
                        Err(e)
                    }
                }
            }
            other => Err(format!(
                "scene: unknown action `{other}` (list, open, switch, close, restore)"
            )
            .into()),
        }
    }

    /// Starts a call from `client` (None: an untagged call, nothing
    /// changes). Puts that client's own document back in front if another
    /// client switched away from it (or opens `pin`, the driver's --doc),
    /// and says what other clients did to it since this client's last
    /// call. Returns a note for the result, if any.
    pub fn begin_call(
        &mut self,
        client: Option<&str>,
        pin: Option<&str>,
        project: Option<Option<std::path::PathBuf>>,
    ) -> Option<String> {
        self.client = client.map(String::from);
        if project.is_some() {
            crate::catalog::set_call_project(project);
        }
        self.derive_if_bare();
        let me = client?;
        self.in_flight += 1;
        if !self.clients.contains_key(me) {
            // Raise the window for a new agent, unless the human is
            // already working in it.
            self.focus_requested = self.viewer_doc.is_none();
        }
        let mut notes = Vec::new();
        let active = self.model.name.clone();
        let mine = pin
            .map(String::from)
            .or_else(|| self.clients.get(me).map(|v| v.doc.clone()));
        let told = self.clients.get(me).is_some_and(|v| v.told_switch);
        if let Some(want) = mine.filter(|w| *w != active) {
            let open = self.others.iter().any(|m| m.name == want);
            if open || pin.is_some() {
                match self.scene("open", Some(&want)) {
                    Ok(_) => {
                        self.mark_autosave(true, true, false);
                        if pin.is_some() {
                            notes.push(format!("switched to `{want}` (--doc)"));
                        } else if !told {
                            notes.push(format!(
                                "the active document was `{active}` (another agent switched to it); switched back to your `{want}`. \
                                 Each agent keeps its own document, so this happens quietly from now on"
                            ));
                            if let Some(v) = self.clients.get_mut(me) {
                                v.told_switch = true;
                            }
                        }
                    }
                    Err(e) => notes.push(format!("could not switch back to your `{want}`: {e}")),
                }
            } else {
                notes.push(format!(
                    "your document `{want}` was closed (by another agent or the human); you are now on `{active}`"
                ));
                self.clients.remove(me);
            }
        }
        // Other clients' edits to this document since we last looked.
        if let Some(v) = self.clients.get(me).filter(|v| v.doc == self.model.name) {
            let h = &self.model.history;
            let theirs: Vec<(usize, &crate::Snapshot)> = h
                .nodes
                .iter()
                .enumerate()
                .skip(v.seen_nodes)
                .filter(|(_, n)| n.author.as_deref() != Some(me))
                .collect();
            if !theirs.is_empty() {
                let mut who: Vec<&str> = theirs
                    .iter()
                    .map(|(_, n)| {
                        n.author
                            .as_deref()
                            .unwrap_or("the human / an untagged call")
                    })
                    .collect();
                who.dedup();
                let shown: Vec<String> = theirs
                    .iter()
                    .rev()
                    .take(4)
                    .rev()
                    .map(|(id, n)| format!("#{id} `{}`", n.label))
                    .collect();
                notes.push(format!(
                    "since your last call, {} made {} edit(s) to `{}`: {}{} (scene_summary shows the result)",
                    who.join(", "),
                    theirs.len(),
                    self.model.name,
                    if theirs.len() > 4 { "… " } else { "" },
                    shown.join("; ")
                ));
            } else if h.current != v.seen_cursor && h.nodes.len() == v.seen_nodes {
                notes.push(format!(
                    "the history cursor of `{}` moved from {} to {} since your last call (someone undid or jumped)",
                    self.model.name,
                    v.seen_cursor.map_or("-".into(), |c| format!("#{c}")),
                    h.current.map_or("-".into(), |c| format!("#{c}"))
                ));
            }
        }
        self.call_doc
            .insert(me.to_string(), self.model.name.clone());
        (!notes.is_empty()).then(|| notes.join("\n"))
    }

    /// Ends a call: remembers the client's document and how much of its
    /// history it has seen.
    pub fn end_call(&mut self, client: Option<&str>) {
        if let Some(p) = crate::catalog::set_call_project(None) {
            self.model.project = p;
        }
        self.touched
            .insert(self.model.name.clone(), std::time::Instant::now());
        if let Some(me) = client.map(String::from) {
            self.in_flight = self.in_flight.saturating_sub(1);
            // Calls overlap: another client may have the active document
            // now; this one's is where its call worked.
            let mine = self.client.as_deref() == Some(me.as_str());
            if mine {
                self.client = None;
            }
            let doc = if mine {
                self.model.name.clone()
            } else {
                self.call_doc
                    .get(&me)
                    .cloned()
                    .unwrap_or_else(|| self.model.name.clone())
            };
            self.call_doc.remove(&me);
            let told_switch = self.clients.get(&me).is_some_and(|v| v.told_switch);
            let (seen_nodes, seen_cursor) = if doc == self.model.name {
                (self.model.history.nodes.len(), self.model.history.current)
            } else {
                self.others
                    .iter()
                    .find(|m| m.name == doc)
                    .map(|m| (m.history.nodes.len(), m.history.current))
                    .unwrap_or((0, None))
            };
            self.clients.insert(
                me,
                ClientView {
                    doc,
                    told_switch,
                    seen_nodes,
                    seen_cursor,
                },
            );
            // Back to the human's document, quietly, once no call is left.
            if self.in_flight == 0
                && let Some(v) = self.viewer_doc.clone()
                && v != self.model.name
            {
                if self.others.iter().any(|m| m.name == v) {
                    let _ = self.scene_impl("switch", Some(&v));
                } else {
                    self.viewer_doc = None;
                }
            }
        }
    }

    /// Notes that the active document matches its catalog file now (just
    /// saved or loaded): closing it while it stays so loses nothing.
    pub fn mark_saved(&mut self) {
        let h = &self.model.history;
        self.saved_state
            .insert(self.model.name.clone(), (h.nodes.len(), h.current));
    }

    /// Closes inactive documents that are saved, unchanged since, and idle
    /// for `idle` (checked at most once a minute). Unsaved work is never
    /// closed. Returns the names closed.
    pub fn close_idle(&mut self, idle: std::time::Duration) -> Vec<String> {
        if self
            .idle_checked
            .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(60))
        {
            return Vec::new();
        }
        self.idle_checked = Some(std::time::Instant::now());
        // Documents restored from the autosave: compare with the catalog
        // file once.
        for m in &self.others {
            if !self.saved_state.contains_key(&m.name) && !self.unsaved_checked.contains(&m.name) {
                if crate::catalog::with_project(m.project.clone(), || {
                    crate::catalog::matches_prefab_file(m)
                }) {
                    self.saved_state
                        .insert(m.name.clone(), (m.history.nodes.len(), m.history.current));
                } else {
                    self.unsaved_checked.insert(m.name.clone());
                }
            }
        }
        let started = self.started;
        let mut closed = Vec::new();
        let (touched, saved) = (&self.touched, &self.saved_state);
        self.others.retain(|m| {
            let last = touched.get(&m.name).copied().unwrap_or(started);
            let unchanged = saved
                .get(&m.name)
                .is_some_and(|k| *k == (m.history.nodes.len(), m.history.current));
            if unchanged && last.elapsed() >= idle {
                closed.push(m.name.clone());
                false
            } else {
                true
            }
        });
        for n in &closed {
            crate::catalog::set_open_doc(n, None);
            self.touched.remove(n);
        }
        if !closed.is_empty() {
            self.mark_autosave(false, true, false);
        }
        closed
    }

    /// The parked document `name` this call means: in the calling
    /// project when the call names one, else the only one of that name
    /// (two projects' documents of one name are refused, listing both).
    fn parked_index(&self, name: &str) -> Result<usize, String> {
        let all: Vec<usize> = (0..self.parked.len())
            .filter(|&i| self.parked[i].name == name)
            .collect();
        let pick: Vec<usize> = match crate::catalog::call_project() {
            Some(want) => all
                .iter()
                .copied()
                .filter(|&i| self.parked[i].project == want)
                .collect(),
            None => all.clone(),
        };
        match (pick.as_slice(), all.is_empty()) {
            ([i], _) => Ok(*i),
            (_, true) => Err(format!(
                "no parked document `{name}` ({})",
                if self.parked.is_empty() {
                    "none are parked".to_string()
                } else {
                    self.parked
                        .iter()
                        .map(|m| m.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            )),
            _ => Err(format!(
                "parked documents named `{name}` exist in {}; call from that project's folder",
                all.iter()
                    .map(|&i| project_label(self.parked[i].project.as_deref()))
                    .collect::<Vec<_>>()
                    .join(" and ")
            )),
        }
    }

    /// The parked documents, one line each, with how to bring one back.
    pub fn parked_note(&self) -> String {
        let mut s = String::from(
            "parked (unsaved work from before the last restart, not open; `scene restore` NAME opens one, `scene close` NAME drops it):\n",
        );
        for m in &self.parked {
            s += &format!(
                "  {} ({} ops, v{}{})\n",
                m.name,
                m.ops.len(),
                m.version,
                m.history
                    .current
                    .and_then(|c| m.history.nodes.get(c))
                    .and_then(|n| n.author.as_deref())
                    .map(|a| format!(", last edit by {a}"))
                    .unwrap_or_default()
            );
        }
        s
    }

    /// Writes the parked documents' autosave.
    pub fn save_parked(&self) {
        if let Err(e) = crate::catalog::save_parked_docs(&self.parked) {
            eprintln!("parked docs autosave failed: {e}");
        }
    }

    /// At startup (`--restore`): every unsaved document the autosave held
    /// is parked instead of opened, and the server starts on an empty
    /// document. Returns the parked names.
    pub fn park_restored(&mut self) -> Vec<String> {
        let mut parked = crate::catalog::load_parked_docs();
        let version = self.model.version + 1;
        let active = std::mem::replace(&mut self.model, Model::named("model"));
        self.model.version = version;
        let mut fresh = std::mem::take(&mut self.others);
        if !(active.ops.is_empty() && active.parts.is_empty()) {
            fresh.insert(0, active);
        }
        let names: Vec<String> = fresh.iter().map(|m| m.name.clone()).collect();
        for n in &names {
            crate::catalog::set_open_doc(n, None);
            self.saved_state.remove(n);
        }
        for m in fresh {
            parked.retain(|p| !(p.name == m.name && p.project == m.project));
            parked.push(m);
        }
        self.parked = parked;
        self.save_parked();
        self.mark_autosave(true, true, false);
        names
    }

    /// After restoring an autosave: closes every document that matches its
    /// catalog file (it reopens from there on demand), so a restart brings
    /// back only unsaved work, not every document that was open. An active
    /// document that goes leaves an empty one in its place. Returns the
    /// closed names.
    pub fn close_saved_documents(&mut self) -> Vec<String> {
        let saved = |m: &Model| {
            m.ops.is_empty() && m.parts.is_empty()
                || crate::catalog::with_project(m.project.clone(), || {
                    crate::catalog::matches_prefab_file(m)
                })
        };
        let mut closed = Vec::new();
        let empty = |m: &Model| m.ops.is_empty() && m.parts.is_empty();
        self.others.retain(|m| {
            let keep = !saved(m);
            if !keep && !empty(m) {
                closed.push(m.name.clone());
            }
            keep
        });
        if saved(&self.model) {
            if !empty(&self.model) {
                closed.push(self.model.name.clone());
            }
            let version = self.model.version + 1;
            self.model = self.others.pop().unwrap_or_else(|| Model::named("model"));
            self.model.version = self.model.version.max(version);
        }
        for n in &closed {
            crate::catalog::set_open_doc(n, None);
        }
        if !closed.is_empty() {
            self.mark_autosave(true, true, false);
        }
        closed
    }

    /// `undo`, but when the step it would revert was made by another client
    /// it refuses (unless `force`): one agent's undo must not silently
    /// throw away another's work in a shared document.
    pub fn undo_checked(&mut self, force: bool) -> CmdResult {
        if !force
            && let Some(me) = self.client.as_deref()
            && let Some(cur) = self.model.history.current
            && let Some(node) = self.model.history.nodes.get(cur)
            && let Some(other) = node.author.as_deref()
            && other != me
        {
            return Err(format!(
                "the last step in `{}` (#{cur} `{}`) was made by another agent ({other}), not you: undo would revert their work. \
                 Find your own step with history and use diff / cherry_pick, or undo with force: true if reverting it is intended",
                self.model.name, node.label
            )
            .into());
        }
        self.undo()
    }

    /// Saves the active model as a catalog prefab, refusing (unless
    /// `force`) when the catalog file changed since this session loaded or
    /// last saved it: two agents (or two servers) saving one prefab must not
    /// silently drop each other's work.
    pub fn save_prefab_checked(
        &mut self,
        description: &str,
        tags: &[&str],
        prompt: &str,
        force: bool,
    ) -> Result<(String, bool), ModelerError> {
        let name = crate::catalog::sanitize(&self.model.name);
        let now = crate::catalog::prefab_file_hash(&name);
        if !force
            && let (Some(base), Some(now)) = (self.catalog_base.get(&name), now)
            && *base != now
        {
            return Err(format!(
                "the catalog entry `{name}` changed since you loaded or saved it (another agent or modeler server saved it): \
                 saving would overwrite their version. Look at it in another document (scene open) or with catalog_describe, \
                 then save_prefab with force: true to overwrite, or set_model_name to save under a new name"
            )
            .into());
        }
        let r = crate::catalog::save_prefab(&self.model, description, tags, prompt)?;
        self.mark_saved();
        if let Some(h) = crate::catalog::prefab_file_hash(&r.0) {
            self.catalog_base.insert(r.0.clone(), h);
        }
        Ok(r)
    }

    /// Names of every open document, the active one first.
    pub fn doc_names(&self) -> Vec<String> {
        std::iter::once(&self.model)
            .chain(&self.others)
            .map(|m| m.name.clone())
            .collect()
    }

    /// Registers every inactive document for prefab lookup (after a restore).
    pub fn register_docs(&self) {
        for m in &self.others {
            crate::catalog::set_open_doc(&m.name, Some(m.clone_without_history()));
        }
    }

    pub fn set_style(&mut self, new: crate::style::Style) -> CmdResult {
        new.check()?;
        let before = self.model_without_history();
        let live: Vec<String> = self.model.ops.iter().map(Self::template_key).collect();
        self.model.templates.retain(|k, _| live.contains(k));
        let full = new.params_under(&self.model.params);
        let mut changed = Vec::new();
        for (key, raw) in &self.model.templates {
            let resolved = crate::style::expand(raw, &new)
                .and_then(|v| crate::params::resolve(&v, &full))
                .map_err(|e| format!("re-styling `{key}`: {e}\n(style unchanged)"))?;
            let mut item = crate::spec::parse_item(0, &resolved)
                .map_err(|e| format!("re-styling `{key}`: {e}\n(style unchanged)"))?;
            self.resolve_pbr(&mut item)
                .map_err(|e| format!("re-styling `{key}`: {e}\n(style unchanged)"))?;
            if let BatchItem::Op(mut op) = item {
                self.resolve_scatter(&mut op)
                    .map_err(|e| format!("re-styling `{key}`: {e}\n(style unchanged)"))?;
                changed.push(op);
            }
        }
        let mut n = 0;
        for op in changed {
            // Every op sharing the template (repeated in-place edits stack,
            // and the last one wins), not just the first.
            let want = serde_json::to_value(&op).ok();
            for slot in self.model.ops.iter_mut().filter(|o| {
                graph::output(o) == graph::output(&op) && graph::kind(o) == graph::kind(&op)
            }) {
                if serde_json::to_value(&*slot).ok() != want {
                    *slot = op.clone();
                    n += 1;
                }
            }
        }
        self.model.style = new;
        if let Err(e) = self.model.refresh() {
            self.restore_model(before);
            return Err(format!("re-deriving restyled ops failed: {e}\n(style unchanged)").into());
        }
        self.commit("style");
        let _ = self.sync_prefabs();
        let reading = self.model.templates.len();
        Ok(format!(
            "style set; re-derived {n} op(s) ({reading} op(s) read params or style tokens)"
        ))
    }

    /// Applies a mixed batch: graph ops and baked edits (hide/show/rename),
    /// each running in its listed position with per-item error reporting.
    /// Graph ops keep the upsert/rollback semantics of `apply_op`.
    pub fn apply_batch(&mut self, items: Vec<BatchItem>) -> CmdResult {
        self.apply_items(items.into_iter().map(|i| (i, None)).collect(), true)
    }

    /// `apply_batch` with an optional parameter template per item: an op item
    /// stores its template (or drops a stale one) so parameter changes can
    /// re-derive it.
    fn apply_items(
        &mut self,
        items: Vec<(BatchItem, Option<serde_json::Value>)>,
        with_summary: bool,
    ) -> CmdResult {
        let items = producers_first(items);
        let mut lines = Vec::with_capacity(items.len());
        for (i, (item, template)) in items.into_iter().enumerate() {
            if let BatchItem::Op(op) = &item {
                let key = Self::template_key(op);
                match template {
                    Some(t) => {
                        self.model.templates.insert(key, t);
                    }
                    None => {
                        self.model.templates.remove(&key);
                    }
                }
            }
            let fail = |e: ModelerError| format!("batch item #{i} failed: {e}");
            let msg = match item {
                // In-place material/transform on a baked part: a baked edit.
                BatchItem::Op(Op::Material {
                    name,
                    input,
                    base_color,
                    roughness,
                    metalness,
                    pbr,
                    flat,
                }) if name == input && !self.derived_by_ops(&name) => {
                    let has_pbr = pbr.is_some();
                    let msg = self
                        .set_material_with(&name, base_color, roughness, metalness, pbr)
                        .map_err(fail)?;
                    if let Some(p) = self.model.parts.iter_mut().find(|p| p.name == name) {
                        if flat {
                            std::sync::Arc::make_mut(p).material.flat = true;
                        } else if has_pbr {
                            std::sync::Arc::make_mut(p).material.flat = false;
                        }
                    }
                    msg
                }
                BatchItem::Op(Op::Transform {
                    name,
                    input,
                    translation,
                    rotation,
                    scale,
                    relative: false,
                    ..
                }) if name == input && !self.derived_by_ops(&name) => self
                    .transform_part(&name, translation, rotation, scale)
                    .map_err(fail)?,
                BatchItem::Op(mut op) => {
                    self.resolve_scatter(&mut op)
                        .map_err(|e| format!("batch item #{i} failed: {e}"))?;
                    let label = Self::op_label(&op);
                    let note = self.scatter_note_for(&op);
                    self.apply_op(op, &label)
                        .map_err(|e| format!("batch item #{i} (`{label}`) failed: {e}"))?;
                    match note {
                        Some((name, want)) => self.scatter_note(&name, want),
                        None => format!("{label} ok"),
                    }
                }
                BatchItem::Delete { name } => {
                    if self.model.ops.iter().any(|o| graph::output(o) == name) {
                        self.op_delete(&name).map_err(fail)?
                    } else {
                        self.delete_part(&name).map_err(fail)?
                    }
                }
                BatchItem::TriMesh {
                    name,
                    verts,
                    indices,
                    normals,
                    uvs,
                } => self
                    .add_tri_mesh(&name, verts, indices, normals, uvs)
                    .map_err(fail)?,
                BatchItem::Move {
                    name,
                    parts,
                    translation,
                    rotation,
                    pivot,
                    on,
                    at,
                    gap,
                    toward,
                    each,
                } => self
                    .move_parts(
                        name,
                        &parts,
                        translation,
                        rotation,
                        pivot,
                        (on, at, gap, toward, each),
                    )
                    .map_err(fail)?,
                BatchItem::Uvs { part, projection } => {
                    self.set_uvs(&part, &projection).map_err(fail)?
                }
                BatchItem::Hide { part } => self
                    .set_visible_glob(&part, false)
                    .map_err(|e| format!("batch item #{i} failed: {e}"))?,
                BatchItem::Show { part } => self
                    .set_visible_glob(&part, true)
                    .map_err(|e| format!("batch item #{i} failed: {e}"))?,
                BatchItem::Rename { part, to } => self
                    .rename_part(&part, &to)
                    .map_err(|e| format!("batch item #{i} failed: {e}"))?,
            };
            lines.push(format!("#{i} {msg}"));
        }
        if with_summary {
            lines.push(self.summary());
        }
        Ok(lines.join("\n"))
    }

    /// True when some op in the graph writes a part called `name`. Edits to
    /// such parts are recorded as graph ops (so they survive re-derivation);
    /// anything else is a baked part and is edited directly.
    fn derived_by_ops(&self, name: &str) -> bool {
        self.model.derived_from_ops
            && self
                .model
                .ops
                .iter()
                .any(|o| graph::outputs(o).iter().any(|n| n == name))
    }

    /// Records a parent transform (`Op::Move`) over every part matching
    /// `patterns` (exact or trailing-`*`): rotation about `pivot` (default:
    /// centre of their combined bottom face, fixed now), then translation.
    /// It applies after the graph, so nothing downstream sees it. Named
    /// moves are upserts: re-issue `name` to adjust.
    #[allow(clippy::type_complexity)]
    pub fn move_parts(
        &mut self,
        name: Option<String>,
        patterns: &[String],
        translation: Option<[f32; 3]>,
        rotation: Option<[f32; 4]>,
        pivot: Option<PivotSpec>,
        (on, at, gap, toward, each): (
            Option<String>,
            Option<[f32; 2]>,
            Option<f32>,
            Option<String>,
            bool,
        ),
    ) -> CmdResult {
        if !self.model.derived_from_ops {
            return Err("move needs an op-built model (this one has baked parts); use transform on each part".into());
        }
        let verb = if on.is_some() { "place" } else { "move" };
        let name = name.unwrap_or_else(|| {
            let n = self
                .model
                .ops
                .iter()
                .filter(|o| matches!(o, Op::Move { .. }))
                .count();
            format!("{verb}_{}", n + 1)
        });
        // Measure on the parts as they are WITHOUT this move (a re-issue
        // replaces it), so pivots and anchors don't drift.
        let parts_now: Vec<Arc<Part>> = if self
            .model
            .ops
            .iter()
            .any(|o| matches!(o, Op::Move { name: n, .. } if *n == name))
        {
            let ops: Vec<Op> = self
                .model
                .ops
                .iter()
                .filter(|o| !matches!(o, Op::Move { name: n, .. } if *n == name))
                .cloned()
                .collect();
            graph::evaluate(&ops)?
        } else {
            self.model.parts.clone()
        };
        let box_of = |pats: &[String]| {
            parts_now
                .iter()
                .filter(|p| {
                    !p.verts.is_empty() && pats.iter().any(|q| crate::name_matches(q, &p.name))
                })
                .fold(None, |acc: Option<(Vec3, Vec3)>, p| {
                    let (lo, hi) = p.bounds();
                    Some(match acc {
                        None => (lo, hi),
                        Some((a, b)) => (a.min(lo), b.max(hi)),
                    })
                })
        };
        let count = parts_now
            .iter()
            .filter(|p| patterns.iter().any(|q| crate::name_matches(q, &p.name)))
            .count();
        let Some((lo, hi)) = box_of(patterns) else {
            return Err(format!("{verb}: no part matches {patterns:?}").into());
        };
        let pivot = match pivot {
            Some(PivotSpec::Point(p)) => p,
            Some(PivotSpec::Anchor(a)) => {
                let (target, words) = match a.split_once(':') {
                    Some((t, w)) => (Some(t.to_string()), w.to_string()),
                    None => (None, a.clone()),
                };
                let (alo, ahi) = match &target {
                    Some(t) => box_of(std::slice::from_ref(t))
                        .ok_or_else(|| format!("{verb}: pivot anchor `{a}`: no part `{t}`"))?,
                    None => (lo, hi),
                };
                anchor_point(&words, alo, ahi).map_err(|e| format!("{verb}: pivot `{a}`: {e}"))?
            }
            None => [(lo.x + hi.x) * 0.5, lo.y, (lo.z + hi.z) * 0.5],
        };
        if let Some(on) = &on
            && !parts_now.iter().any(|p| &p.name == on)
        {
            return Err(format!("place: no part `{on}` to place on").into());
        }
        let label = format!("{verb} `{name}`");
        self.apply_op(
            Op::Move {
                name: name.clone(),
                parts: patterns.to_vec(),
                translation,
                rotation,
                pivot,
                on: on.clone(),
                at,
                gap,
                toward: toward.clone(),
                each,
            },
            &label,
        )?;
        let report = move_report(&parts_now, &self.model.parts, patterns);
        Ok(match on {
            Some(on) => format!(
                "place `{name}`: {count} part(s) {} `{on}` (re-issue name `{name}` to adjust){report}",
                match toward.as_deref() {
                    None | Some("down") => "dropped onto".to_string(),
                    Some("up") => "raised against the underside of".to_string(),
                    Some(t) => format!("moved {t} against"),
                }
            ),
            None => format!(
                "move `{name}`: {count} part(s) as one body about [{:.4}, {:.4}, {:.4}] (re-issue name `{name}` to adjust){report}",
                pivot[0], pivot[1], pivot[2]
            ),
        })
    }

    /// [`Self::set_visible`] on one part, or on every part matching a
    /// trailing-`*` prefix (`"book*"`, `"cut_*"`).
    pub fn set_visible_glob(&mut self, pattern: &str, visible: bool) -> CmdResult {
        if !pattern.contains('*') {
            return self.set_visible(pattern, visible);
        }
        let names: Vec<String> = self
            .model
            .parts
            .iter()
            .filter(|p| crate::name_matches(pattern, &p.name) && p.visible != visible)
            .map(|p| p.name.clone())
            .collect();
        if names.is_empty()
            && !self
                .model
                .parts
                .iter()
                .any(|p| crate::name_matches(pattern, &p.name))
        {
            return Err(format!("no part matches `{pattern}`").into());
        }
        for n in &names {
            self.set_visible(n, visible)?;
        }
        Ok(format!(
            "{} part(s) {} by `{pattern}`",
            names.len(),
            if visible { "shown" } else { "hidden" }
        ))
    }

    /// Shows or hides a part. Hidden parts stay in the model and keep
    /// deriving from ops — they are just skipped by the render and the glTF
    /// export. For op-derived parts this records a `Visibility` op, so the hide
    /// survives re-derivation; baked parts are edited directly.
    pub fn set_visible(&mut self, name: &str, visible: bool) -> CmdResult {
        if self.derived_by_ops(name) {
            let verb = if visible { "shown" } else { "hidden" };
            self.apply_op(
                Op::Visibility {
                    name: name.into(),
                    input: name.into(),
                    visible,
                },
                &format!("{verb} `{name}`"),
            )?;
            return Ok(format!("part `{name}` {verb}"));
        }
        let Some(p) = self.model.part_mut(name) else {
            return Err(ModelerError::NoPart(name.to_string()));
        };
        p.visible = visible;
        let verb = if visible { "shown" } else { "hidden" };
        self.commit(&format!("{verb} `{name}`"));
        Ok(format!("part `{name}` {verb}"))
    }

    /// The full parameter dump of one op, by output part name.
    pub fn describe_op(&self, name: &str) -> CmdResult {
        graph::describe_one(&self.model.ops, name)
            .ok_or_else(|| ModelerError::Invalid(format!("no op with output `{name}`")))
    }

    /// Re-derives the graph and diffs it against the cached parts (see
    /// []).
    pub fn check_rebuild(&self) -> CmdResult {
        crate::verify::check_rebuild(&self.model)
    }

    /// Exact visible-part-list assertion (see []).
    pub fn expect_parts(&self, expected: &[String]) -> CmdResult {
        crate::verify::expect_parts(&self.model, expected)
    }

    /// World-bounds assertion (see []).
    pub fn expect_bounds(
        &self,
        min: [Option<f32>; 3],
        max: [Option<f32>; 3],
        tol: f32,
    ) -> CmdResult {
        crate::verify::expect_bounds(&self.model, min, max, tol)
    }

    /// Per-part mesh validation (see []).
    /// Runs the checks in `spec` (all of validate + rebuild when `spec` is
    /// empty). Returns (all passed, one line per check).
    pub fn verify(&self, spec: &VerifySpec) -> (bool, String) {
        if let Some(inner) = &spec.expect {
            return self.verify(inner);
        }
        let checks = self.verify_checks(spec);
        let ok = checks.iter().all(|c| c.1);
        let lines: Vec<String> = checks
            .iter()
            .map(|(name, pass, detail)| {
                format!("{} {name}: {detail}", if *pass { "PASS" } else { "FAIL" })
            })
            .collect();
        (ok, lines.join("\n"))
    }

    /// The checks behind [`Self::verify`] as (name, passed, detail).
    pub fn verify_checks(&self, spec: &VerifySpec) -> Vec<(String, bool, String)> {
        // Pair contacts, found once and shared by grounded and supported.
        let contacts_cell = std::cell::OnceCell::new();
        let contacts = || contacts_cell.get_or_init(|| crate::contact::Contacts::new(&self.model));
        let scope: Vec<String> = spec.scope.clone().unwrap_or_default();
        let in_scope =
            |n: &str| scope.is_empty() || scope.iter().any(|p| crate::name_matches(p, n));
        // Per-part checks on just the scoped parts (graph kept for diagnosis).
        let scoped_model = (!scope.is_empty()).then(|| {
            let mut m = Model::named(&self.model.name);
            m.ops = self.model.ops.clone();
            m.parts = self
                .model
                .parts
                .iter()
                .filter(|p| in_scope(&p.name))
                .cloned()
                .collect();
            m
        });
        let view = scoped_model.as_ref().unwrap_or(&self.model);
        let empty = spec.parts.is_none()
            && spec.includes.is_none()
            && spec.grounded.is_none()
            && spec.bounds.is_none()
            && spec.validate.is_none()
            && spec.rebuild.is_none()
            && spec.stable.is_none()
            && spec.supported.is_none()
            && spec.rests.is_none()
            && spec.collisions.is_none()
            && spec.self_intersections.is_none()
            && spec.thickness.is_none()
            && spec.weights.is_none()
            && spec.animation.is_none()
            && spec.materials.is_none();
        let mut out = Vec::new();
        let mut record = |name: &str, r: Result<String, String>| match r {
            Ok(t) => out.push((name.to_string(), true, t)),
            Err(e) => out.push((name.to_string(), false, e)),
        };
        if let Some(parts) = &spec.parts {
            record("parts", self.expect_parts(parts).map_err(|e| e.to_string()));
        }
        if spec.weights == Some(true) {
            record("weights", crate::anim::check::weights(view));
        }
        if let Some(a) = &spec.animation {
            for (name, pass, detail) in crate::anim::check::animation(&self.model, a, &scope) {
                record(&name, if pass { Ok(detail) } else { Err(detail) });
            }
        }
        if let Some(w) = &spec.includes {
            record(
                "includes",
                crate::verify::expect_includes(&self.model, w).map_err(|e| e.to_string()),
            );
        }
        let strict = match &spec.grounded {
            Some(GroundedSpec::Mode(m)) if m == "strict" => Some(true),
            Some(GroundedSpec::Mode(m)) => {
                record(
                    "grounded",
                    Err(format!("grounded: `{m}` (use true or \"strict\")")),
                );
                None
            }
            Some(GroundedSpec::On(true)) => Some(false),
            _ => None,
        };
        if let Some(strict) = strict {
            let (floating, held) = crate::contact::grounding_in(contacts(), &self.model, 0.2);
            let floating: Vec<String> = floating.into_iter().filter(|n| in_scope(n)).collect();
            let deep: Vec<String> = held
                .into_iter()
                .filter(|(n, _)| in_scope(n))
                .map(|(n, through)| {
                    format!(
                        "`{n}` (through {})",
                        crate::compact_names(&through).join(", ")
                    )
                })
                .collect();
            let deep_txt = if deep.is_empty() {
                String::new()
            } else {
                format!(
                    "; held up only by passing deep into another part: {} (a collision holding it up? verify grounded: \"strict\" fails these)",
                    deep.join(", ")
                )
            };
            if !floating.is_empty() {
                record(
                    "grounded",
                    Err(format!(
                        "floating (not connected to the ground through touching parts): {}{deep_txt}",
                        crate::compact_names(&floating).join(", ")
                    )),
                );
            } else if strict && !deep.is_empty() {
                record(
                    "grounded",
                    Err(deep_txt.trim_start_matches("; ").to_string()),
                );
            } else {
                record(
                    "grounded",
                    Ok(format!(
                        "every {}visible part reaches the ground through touching parts{deep_txt}",
                        if scope.is_empty() { "" } else { "scoped " }
                    )),
                );
            }
        }
        if let Some(b) = &spec.bounds {
            record(
                "bounds",
                crate::verify::expect_bounds(view, b.min, b.max, b.tol).map_err(|e| e.to_string()),
            );
        }
        if spec.validate.unwrap_or(empty) {
            let v = crate::verify::validate(view);
            if v.starts_with("all parts") || v.starts_with("no parts") {
                record("validate", Ok(v));
            } else {
                record("validate", Err(v.replace('\n', "; ")));
            }
        }
        if spec.self_intersections == Some(true) || spec.thickness.is_some() {
            use rayon::prelude::*;
            let parts: Vec<&crate::Part> = view
                .parts
                .iter()
                .map(|p| p.as_ref())
                .filter(|p| p.visible && !p.verts.is_empty() && !p.cards)
                .collect();
            if spec.self_intersections == Some(true) {
                let bad: Vec<String> = parts
                    .par_iter()
                    .filter_map(|p| {
                        let (n, at) = crate::organic::self_intersections(p);
                        (n > 0).then(|| {
                            let a = at.first().copied().unwrap_or_default();
                            format!(
                                "`{}` ({n} crossing triangle pairs, e.g. at [{:.3}, {:.3}, {:.3}])",
                                p.name, a.x, a.y, a.z
                            )
                        })
                    })
                    .collect();
                record(
                    "self_intersections",
                    if bad.is_empty() {
                        Ok(format!(
                            "no part's surface passes through itself ({} checked)",
                            parts.len()
                        ))
                    } else {
                        Err(format!(
                            "surfaces passing through themselves: {} (relax it, lower the deform/warp, or check with agent_views style \"curvature\")",
                            bad.join(", ")
                        ))
                    },
                );
            }
            if let Some(min) = spec.thickness {
                let mut rows: Vec<(String, crate::organic::Thickness)> = parts
                    .par_iter()
                    .filter_map(|p| crate::organic::thickness(p, 4000).map(|t| (p.name.clone(), t)))
                    .collect();
                rows.sort_by(|a, b| a.1.min.total_cmp(&b.1.min));
                let bad: Vec<String> = rows
                    .iter()
                    .filter(|(_, t)| t.min < min)
                    .map(|(n, t)| {
                        format!(
                            "`{n}` {:.4} m at [{:.3}, {:.3}, {:.3}]",
                            t.min, t.at.x, t.at.y, t.at.z
                        )
                    })
                    .collect();
                record(
                    "thickness",
                    if bad.is_empty() {
                        Ok(match rows.first() {
                            Some((n, t)) => format!(
                                "every part is at least {min} m thick (thinnest `{n}` {:.4} m)",
                                t.min
                            ),
                            None => "no solid parts to measure".into(),
                        })
                    } else {
                        Err(format!("thinner than {min} m: {}", bad.join(", ")))
                    },
                );
            }
        }
        if spec.rebuild.unwrap_or(empty) {
            record("rebuild", self.check_rebuild().map_err(|e| e.to_string()));
        }
        if spec.supported == Some(true) {
            let bad: Vec<(String, Vec<String>)> =
                crate::contact::unsupported_in(contacts(), &self.model)
                    .into_iter()
                    .filter(|(n, _)| in_scope(n))
                    .collect();
            if bad.is_empty() {
                record(
                    "supported",
                    Ok("every visible part is on the ground, attached, or rests on something below its centre of mass".into()),
                );
            } else {
                let list: Vec<String> = bad
                    .iter()
                    .map(|(n, t)| {
                        if t.is_empty() {
                            format!("`{n}` touches nothing")
                        } else {
                            format!(
                                "`{n}` only touches {} at/above its centre of mass",
                                t.join(", ")
                            )
                        }
                    })
                    .collect();
                record(
                    "supported",
                    Err(format!("unsupported: {}", list.join("; "))),
                );
            }
        }
        if let Some(pairs) = &spec.rests {
            for [a, b] in pairs {
                record(
                    &format!("rests {a} on {b}"),
                    crate::contact::rests_on(&self.model, a, b),
                );
            }
        }
        let coll = match &spec.collisions {
            Some(CollisionSpec::On(true)) => Some((Vec::new(), 0.2)),
            Some(CollisionSpec::With { allow, max }) => Some((allow.clone(), max.unwrap_or(0.2))),
            _ => None,
        };
        if let Some((allow, max)) = coll {
            let report = crate::contact::collisions(&self.model, &allow, max, &scope);
            let (bad, joints) = (&report.bad, &report.joints);
            // What each allow rule excused beyond `max`, deepest first, and a
            // note when a catch-all rule hides a deep overlap.
            let mut excused_txt = String::new();
            for (rule, list) in &report.excused {
                if list.is_empty() {
                    continue;
                }
                let deepest = list.iter().max_by(|a, b| a.depth.total_cmp(&b.depth));
                if let Some(d) = deepest {
                    excused_txt += &format!(
                        "\n  allow {} excused {} pair(s); deepest `{}`/`{}` {:.0}%, {:.1} mm",
                        rule.describe(),
                        list.len(),
                        d.a,
                        d.b,
                        d.fraction * 100.0,
                        d.depth * 1000.0
                    );
                    if rule.is_catch_all() {
                        excused_txt += " (note: a catch-all `*` allow hides real collisions: name the actual partner, or add a depth limit)";
                    }
                }
            }
            let joints_txt = if joints.is_empty() {
                String::new()
            } else {
                let list: Vec<String> = joints
                    .iter()
                    .take(8)
                    .map(|o| format!("{}/{} {:.0}%", o.a, o.b, o.fraction * 100.0))
                    .collect();
                format!(
                    "; {} joint(s): {}{}",
                    joints.len(),
                    list.join(", "),
                    if joints.len() > 8 { ", ..." } else { "" }
                )
            };
            if bad.is_empty() {
                record(
                    "collisions",
                    Ok(format!(
                        "no part is more than {:.0}% inside another{joints_txt}{excused_txt}",
                        max * 100.0
                    )),
                );
            } else {
                let list: Vec<String> = bad
                    .iter()
                    .map(|o| match crate::contact::diagnose(&self.model, o) {
                        Some(why) => format!("{} [{why}]", o.describe()),
                        None => o.describe(),
                    })
                    .collect();
                record(
                    "collisions",
                    Err(format!(
                        "{} pair(s) pass through each other: {} (intended joints go in `allow: [[a, b]]`, or `{{\"pair\": [a, b], \"depth\": 0.003}}` for shallow ones){joints_txt}{excused_txt}",
                        bad.len(),
                        list.join("; ")
                    )),
                );
            }
        }
        let pbr = match &spec.materials {
            None | Some(GroundedSpec::On(false)) => None,
            Some(GroundedSpec::On(true)) => Some(false),
            Some(GroundedSpec::Mode(m)) if m == "pbr" => Some(true),
            Some(GroundedSpec::Mode(m)) => {
                record(
                    "materials",
                    Err(format!("materials: `{m}`: use true or \"pbr\"")),
                );
                None
            }
        };
        if let Some(pbr) = pbr {
            let (bare, flat) = material_gaps(view);
            let mut bad = Vec::new();
            if !bare.is_empty() {
                bad.push(format!(
                    "{} part(s) on the default material: {}",
                    bare.len(),
                    fold(&bare)
                ));
            }
            if pbr && !flat.is_empty() {
                bad.push(format!(
                    "{} part(s) with a flat colour, no pbr material: {}",
                    flat.len(),
                    fold(&flat)
                ));
            }
            if bad.is_empty() {
                record(
                    "materials",
                    Ok(if pbr {
                        "every visible part has a pbr material".into()
                    } else {
                        format!(
                            "every visible part has a material ({} flat colour, the rest pbr)",
                            flat.len()
                        )
                    }),
                );
            } else {
                record(
                    "materials",
                    Err(format!(
                        "{} (give each a material item; pbr: a procedural material, see material_ops)",
                        bad.join("; ")
                    )),
                );
            }
        }
        if spec.stable == Some(true) {
            let (text, json) = crate::inspect::mass(view, &Default::default(), 1000.0);
            let first = text.lines().nth(1).unwrap_or("").to_string();
            if json["stability"]["stable"].as_bool() == Some(true) {
                record("stable", Ok(first));
            } else {
                record("stable", Err(first));
            }
        }
        out
    }

    /// Checks as JSON: [{"check", "pass", "detail"}].
    pub fn checks_json(checks: &[(String, bool, String)]) -> serde_json::Value {
        serde_json::Value::Array(
            checks
                .iter()
                .map(|(c, p, d)| serde_json::json!({"check": c, "pass": p, "detail": d}))
                .collect(),
        )
    }

    /// The scene summary as structured data: model, bounds, params, parts
    /// (visibility, triangle count, world bounds, material), ops (one line
    /// each), warnings and annotations.
    pub fn summary_json(&self) -> serde_json::Value {
        use serde_json::json;
        let b = |v: bevy::math::Vec3| [v.x, v.y, v.z];
        json!({
            "model": self.model.name,
            "version": self.model.version,
            "bounds": self.model.bounds().map(|(lo, hi)| json!({"min": b(lo), "max": b(hi)})),
            "params": self.model.params,
            "parts": self.model.parts.iter().map(|p| {
                let (lo, hi) = p.bounds();
                json!({
                    "name": p.name,
                    "visible": p.visible,
                    "tris": p.tri_count(),
                    "bounds": {"min": b(lo), "max": b(hi)},
                    "color": p.material.base_color,
                    "roughness": p.material.roughness,
                    "metalness": p.material.metalness,
                })
            }).collect::<Vec<_>>(),
            "ops": self.model.ops.iter().map(Self::op_label).collect::<Vec<_>>(),
            "warnings": self.lints(),
            "annotations": self.model.annotations.iter().map(|a| json!({
                "id": a.id, "part": a.part, "text": a.text, "face": a.face, "vertex": a.vertex,
                "group": a.group, "material": a.material, "node": a.node,
                "resolved": a.resolved.as_ref().map(|r| json!({"reply": r.reply, "by": r.by})),
            })).collect::<Vec<_>>(),
        })
    }

    /// The compact structured result of an edit: the prose (which already
    /// lists what changed and every warning) plus the model's headline
    /// numbers, not every part and op (`summary: "full"` for those).
    pub fn brief_json(&self, text: &str) -> serde_json::Value {
        let b = |v: bevy::math::Vec3| [v.x, v.y, v.z];
        serde_json::json!({
            "model": self.model.name,
            "version": self.model.version,
            "parts": self.model.parts.len(),
            "ops": self.model.ops.len(),
            "bounds": self.model.bounds().map(|(lo, hi)| serde_json::json!({"min": b(lo), "max": b(hi)})),
            "text": text,
        })
    }

    pub fn validate(&self) -> String {
        crate::verify::validate(&self.model)
    }

    /// Deterministic scene warnings (see []).
    /// Marks autosaves as pending; [`Session::flush_autosave`] writes them.
    pub fn mark_autosave(&mut self, model: bool, docs: bool, materials: bool) {
        let d = &mut self.autosave_due;
        d.model |= model;
        d.docs |= docs;
        d.materials |= materials;
        if model || docs || materials {
            d.since = Some(std::time::Instant::now());
        }
    }

    /// Writes the pending autosaves once edits have paused for
    /// [`AUTOSAVE_DELAY`], or now with `force` (exit). Op-derived parts are
    /// left out of the files (they re-derive on restore) without cloning the
    /// model: they are moved out for the write and back.
    pub fn flush_autosave(&mut self, force: bool) {
        let Some(since) = self.autosave_due.since else {
            return;
        };
        if !force && since.elapsed() < AUTOSAVE_DELAY {
            return;
        }
        let due = std::mem::take(&mut self.autosave_due);
        if due.model {
            // Parts out for the write (op-derived ones re-derive on restore),
            // history appended to the journal rather than rewritten.
            let parts = if self.model.parts_derive_from_ops() {
                Some(std::mem::take(&mut self.model.parts))
            } else {
                None
            };
            if let Err(e) =
                crate::catalog::save_session_journaled(&mut self.model, &mut self.journal)
            {
                eprintln!("session autosave failed: {e}");
            }
            if let Some(p) = parts {
                self.model.parts = p;
            }
        }
        if due.docs {
            let taken: Vec<_> = self
                .others
                .iter_mut()
                .map(|m| {
                    if m.parts_derive_from_ops() {
                        Some(std::mem::take(&mut m.parts))
                    } else {
                        None
                    }
                })
                .collect();
            if let Err(e) = crate::catalog::save_open_docs(&mut self.others, &mut self.journal) {
                eprintln!("open docs autosave failed: {e}");
            }
            for (m, p) in self.others.iter_mut().zip(taken) {
                if let Some(p) = p {
                    m.parts = p;
                }
            }
        }
        if due.materials {
            self.materials.autosave();
        }
    }

    pub fn lints(&self) -> Vec<String> {
        crate::verify::lints(&self.model)
    }

    pub fn add_primitive(
        &mut self,
        kind: PrimitiveKind,
        size: [f32; 3],
        translation: [f32; 3],
        name: &str,
    ) -> CmdResult {
        let mut part = crate::primitives::primitive(kind, size, name).map_err(|e| e.to_string())?;
        part.translation = translation;
        self.model.parts.push(std::sync::Arc::new(part));
        self.model.mark_baked();
        self.commit(&format!("add {kind} `{name}`"));
        Ok(format!("added {kind} `{name}` at {translation:?}"))
    }

    /// Raw triangle mesh. Auto normals + box UV projection when omitted.
    pub fn add_tri_mesh(
        &mut self,
        name: &str,
        verts: Vec<[f32; 3]>,
        indices: Vec<u32>,
        normals: Option<Vec<[f32; 3]>>,
        uvs: Option<Vec<[f32; 2]>>,
    ) -> CmdResult {
        if verts.len() > u32::MAX as usize || !indices.len().is_multiple_of(3) {
            return Err("indices must be a multiple of 3".into());
        }
        if indices.iter().any(|i| *i as usize >= verts.len()) {
            return Err("index out of range".into());
        }
        let mut part = Part::new(name);
        part.verts = verts;
        part.indices = indices;
        crate::uv::ensure_normals(&mut part);
        crate::uv::ensure_uvs(&mut part, Some("box")).map_err(|e| e.to_string())?;
        if let Some(n) = normals {
            part.normals = n;
        }
        if let Some(u) = uvs {
            part.uvs = u;
        }
        let tris = part.tri_count();
        self.model.parts.push(std::sync::Arc::new(part));
        self.commit(&format!("add tri mesh `{name}`"));
        Ok(format!("added tri mesh `{name}` ({tris} tris)"))
    }

    pub fn boolean(&mut self, op: BoolOp, a: &str, b: &str, name: Option<&str>) -> CmdResult {
        let out = crate::csg::boolean(&self.model, op, a, b, name)?;
        self.model.parts.retain(|p| p.name != a && p.name != b);
        self.model.parts.push(std::sync::Arc::new(out));
        self.commit(&format!("{op}({a}, {b})"));
        Ok(format!("{op}({a}, {b}) ok"))
    }

    pub fn transform_part(
        &mut self,
        part: &str,
        translation: Option<[f32; 3]>,
        rotation: Option<[f32; 4]>,
        scale: Option<[f32; 3]>,
    ) -> CmdResult {
        if self.derived_by_ops(part) {
            self.op_transform(part, part, translation, rotation, scale)?;
            return Ok(format!("transformed `{part}`"));
        }
        let p = self
            .model
            .part_mut(part)
            .ok_or_else(|| ModelerError::NoPart(part.to_string()))?;
        if let Some(t) = translation {
            p.translation = t;
        }
        if let Some(r) = rotation {
            p.rotation = r;
        }
        if let Some(s) = scale {
            p.scale = s;
        }
        self.model.mark_baked();
        self.commit(&format!("transform `{part}`"));
        Ok(format!("transformed `{part}`"))
    }

    pub fn delete_part(&mut self, part: &str) -> CmdResult {
        let before = self.model.parts.len();
        self.model.parts.retain(|p| p.name != part);
        if self.model.parts.len() == before {
            Err(ModelerError::NoPart(part.to_string()))
        } else {
            self.model.mark_baked();
            self.commit(&format!("delete `{part}`"));
            Ok(format!("deleted `{part}`"))
        }
    }

    pub fn rename_part(&mut self, part: &str, to: &str) -> CmdResult {
        if self.derived_by_ops(part) {
            self.apply_op(
                Op::Rename {
                    name: to.into(),
                    input: part.into(),
                },
                &format!("rename `{part}` -> `{to}`"),
            )?;
            return Ok(format!("renamed `{part}` -> `{to}`"));
        }
        let p = self
            .model
            .part_mut(part)
            .ok_or_else(|| ModelerError::NoPart(part.to_string()))?;
        p.name = to.to_string();
        self.model.mark_baked();
        self.commit(&format!("rename `{part}` -> `{to}`"));
        Ok(format!("renamed `{part}` -> `{to}`"))
    }

    pub fn set_material(
        &mut self,
        part: &str,
        base_color: Option<[f32; 4]>,
        roughness: Option<f32>,
        metalness: Option<f32>,
    ) -> CmdResult {
        self.set_material_with(part, base_color, roughness, metalness, None)
    }

    /// [`Self::set_material`] with an optional procedural material (its
    /// graph already resolved).
    pub fn set_material_with(
        &mut self,
        part: &str,
        base_color: Option<[f32; 4]>,
        roughness: Option<f32>,
        metalness: Option<f32>,
        pbr: Option<crate::material::PbrRef>,
    ) -> CmdResult {
        if self.derived_by_ops(part) {
            self.apply_op(
                Op::Material {
                    name: part.into(),
                    input: part.into(),
                    base_color,
                    roughness,
                    metalness,
                    pbr,
                    flat: false,
                },
                &format!("op material `{part}` <- `{part}`"),
            )?;
            return Ok(format!("material on `{part}` updated"));
        }
        let p = self
            .model
            .part_mut(part)
            .ok_or_else(|| ModelerError::NoPart(part.to_string()))?;
        if let Some(r) = pbr {
            r.graph()?;
            p.material.base_color = [1.0; 4];
            p.material.roughness = 1.0;
            p.material.metalness = 1.0;
            p.material.pbr = Some(r);
            crate::material::pbr::map_part(p)?;
        }
        if let Some(c) = base_color {
            p.material.base_color = c;
        }
        if let Some(r) = roughness {
            p.material.roughness = r;
        }
        if let Some(m) = metalness {
            p.material.metalness = m;
        }
        self.model.mark_baked();
        self.commit(&format!("material on `{part}`"));
        Ok(format!("material on `{part}` updated"))
    }

    pub fn set_uvs(&mut self, part: &str, projection: &str) -> CmdResult {
        let p = self
            .model
            .part_mut(part)
            .ok_or_else(|| ModelerError::NoPart(part.to_string()))?;
        crate::uv::project(p, projection).map_err(|e| e.to_string())?;
        self.model.mark_baked();
        self.commit(&format!("uvs on `{part}`"));
        Ok(format!("uvs on `{part}` set to {projection}"))
    }

    pub fn undo(&mut self) -> CmdResult {
        let (id, mut model) = self.model.history.undo().ok_or("nothing to undo")?;
        model.restore_parts()?;
        model.history = std::mem::take(&mut self.model.history);
        self.model = model;
        Ok(format!("undone to #{id}, model at v{}", self.model.version))
    }

    pub fn redo(&mut self) -> CmdResult {
        let (id, mut model) = self.model.history.redo().ok_or("nothing to redo")?;
        model.restore_parts()?;
        model.history = std::mem::take(&mut self.model.history);
        self.model = model;
        Ok(format!("redone to #{id}, model at v{}", self.model.version))
    }

    /// Jump the cursor anywhere in the history tree. Editing after a jump
    /// branches: the old future stays reachable as a sibling subtree.
    pub fn jump(&mut self, id: usize) -> CmdResult {
        let (_, mut model) = self.model.history.jump(id)?;
        model.restore_parts()?;
        model.history = std::mem::take(&mut self.model.history);
        self.model = model;
        Ok(format!(
            "jumped to #{id}, model at v{} (editing here branches)",
            self.model.version
        ))
    }

    /// Template key of an op: in-place ops share output names, so the kind
    /// is part of the identity.
    pub fn template_key(op: &Op) -> String {
        format!("{}:{}", graph::kind(op), graph::output(op))
    }

    /// The model at history node `id`, or the live model for `None`.
    fn model_at(&self, id: Option<usize>) -> Result<&Model, ModelerError> {
        match id {
            None => Ok(&self.model),
            Some(id) => self
                .model
                .history
                .nodes
                .get(id)
                .map(|n| &n.model)
                .ok_or_else(|| format!("no history node #{id} (see history)").into()),
        }
    }

    /// Ops keyed by (output, kind) — the upsert identity.
    fn op_index(model: &Model) -> Vec<((String, &'static str), &Op)> {
        model
            .ops
            .iter()
            .map(|o| ((graph::output(o).to_string(), graph::kind(o)), o))
            .collect()
    }

    /// Ops in `to` that are new or different from `from`, in `to`'s order.
    fn ops_changed_between(from: &Model, to: &Model) -> Vec<Op> {
        let old = Self::op_index(from);
        Self::op_index(to)
            .into_iter()
            .filter(|(k, op)| {
                old.iter().find(|(ok, _)| ok == k).is_none_or(|(_, o)| {
                    serde_json::to_value(o).ok() != serde_json::to_value(op).ok()
                })
            })
            .map(|(_, op)| op.clone())
            .collect()
    }

    /// What changed in the op graph (and params) from history node `from` to
    /// node `to` (default: the live model): `+` added, `-` removed, `~`
    /// changed, each with the op's full parameters.
    pub fn diff(&self, from: usize, to: Option<usize>) -> CmdResult {
        let (a, b) = (self.model_at(Some(from))?, self.model_at(to)?);
        let (ia, ib) = (Self::op_index(a), Self::op_index(b));
        let mut lines = Vec::new();
        for (k, op) in &ib {
            match ia.iter().find(|(ok, _)| ok == k) {
                None => lines.push(format!("+ {}", Self::op_label(op))),
                Some((_, old)) => {
                    if serde_json::to_value(old).ok() != serde_json::to_value(op).ok() {
                        lines.push(format!(
                            "~ {}\n    was: {}",
                            Self::op_label(op),
                            Self::op_label(old)
                        ));
                    }
                }
            }
        }
        for (k, op) in &ia {
            if !ib.iter().any(|(nk, _)| nk == k) {
                lines.push(format!("- {}", Self::op_label(op)));
            }
        }
        for (k, v) in &b.params {
            match a.params.get(k) {
                None => lines.push(format!("+ param {k}={v}")),
                Some(old) if old != v => lines.push(format!("~ param {k}={v} (was {old})")),
                _ => {}
            }
        }
        for k in a.params.keys().filter(|k| !b.params.contains_key(*k)) {
            lines.push(format!("- param {k}"));
        }
        let to_label = to.map_or("live".to_string(), |t| format!("#{t}"));
        if lines.is_empty() {
            return Ok(format!("#{from} -> {to_label}: no op or param changes"));
        }
        Ok(format!(
            "#{from} -> {to_label}: {} change(s)\n{}",
            lines.len(),
            lines.join("\n")
        ))
    }

    /// Replays ops from history node `from` onto the live model: every op
    /// there that is new or different from the live graph (or just `only`,
    /// by output name), with their parameter templates and any parameters
    /// the live model lacks. The way back after a `jump`: re-apply work from
    /// the abandoned branch without retyping it.
    pub fn cherry_pick(&mut self, from: usize, only: Option<Vec<String>>) -> CmdResult {
        let src = self.model_at(Some(from))?.clone();
        let mut picks = Self::ops_changed_between(&self.model, &src);
        // Params that differ at `from`; they can be picked by name too.
        let param_diff: crate::params::Params = src
            .params
            .iter()
            .filter(|(k, v)| self.model.params.get(*k) != Some(*v))
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        let mut params = crate::params::Params::new();
        if let Some(names) = &only {
            let missing: Vec<&String> = names
                .iter()
                .filter(|n| {
                    !picks.iter().any(|o| graph::output(o) == n.as_str())
                        && !param_diff.contains_key(n.as_str())
                })
                .collect();
            if !missing.is_empty() {
                let mut available: Vec<String> =
                    picks.iter().map(|o| graph::output(o).to_string()).collect();
                available.sort();
                available.dedup();
                available.extend(param_diff.keys().map(|k| format!("param {k}")));
                return Err(format!(
                    "node #{from} has no new/changed op or param named {} (available: {})",
                    missing
                        .iter()
                        .map(|n| format!("`{n}`"))
                        .collect::<Vec<_>>()
                        .join(", "),
                    if available.is_empty() {
                        "none".to_string()
                    } else {
                        available.join(", ")
                    }
                )
                .into());
            }
            picks.retain(|o| names.iter().any(|n| n == graph::output(o)));
            for n in names {
                if let Some(v) = param_diff.get(n) {
                    params.insert(n.clone(), *v);
                }
            }
        } else {
            params = param_diff.clone();
        }
        // A picked op brings the param values it was derived with, so the
        // live graph stays consistent with its params.
        let templates: Vec<Option<serde_json::Value>> = picks
            .iter()
            .map(|op| src.templates.get(&Self::template_key(op)).cloned())
            .collect();
        for t in templates.iter().flatten() {
            for r in crate::params::refs(t) {
                if let Some(v) = src.params.get(&r) {
                    params.insert(r, *v);
                }
            }
        }
        if picks.is_empty() && params.is_empty() {
            return Ok(format!(
                "nothing to pick: node #{from} has no ops or params that differ from the live graph"
            ));
        }
        let mut out = Vec::new();
        self.batch(|s| {
            if !params.is_empty() {
                out.push(s.apply_values(Some(params.clone()), Vec::new())?);
            }
            if !picks.is_empty() {
                let items = picks
                    .into_iter()
                    .zip(templates)
                    .map(|(op, t)| (BatchItem::Op(op), t))
                    .collect();
                out.push(s.apply_items(items, false)?);
            }
            Ok(())
        })?;
        Ok(format!("cherry-picked from #{from}:\n{}", out.join("\n")))
    }

    /// Prunes old history, keeping the newest `keep` snapshots, the cursor,
    /// the root and every branch point. Ids are renumbered.
    pub fn history_prune(&mut self, keep: usize) -> CmdResult {
        let removed = self.model.history.prune(keep);
        Ok(format!(
            "pruned {removed} history node(s); {} remain (ids renumbered)\n{}",
            self.model.history.nodes.len(),
            self.model.history.describe()
        ))
    }

    /// The history tree, one line per snapshot: the newest `limit` (default
    /// 30) plus the cursor line, with a count of the elided older nodes.
    pub fn history(&self, limit: Option<usize>) -> String {
        let all = self.model.history.describe();
        let lines: Vec<&str> = all.lines().collect();
        let limit = limit.unwrap_or(30).max(1);
        if lines.len() <= limit {
            return all;
        }
        let tail_start = lines.len() - limit;
        let mut out = vec![format!(
            "{} history nodes; showing the newest {limit} (history limit=N for more)",
            lines.len()
        )];
        if let Some(cur) = lines[..tail_start].iter().find(|l| l.ends_with("<-")) {
            out.push(format!("{cur}  (cursor)"));
            out.push("…".into());
        }
        out.extend(lines[tail_start..].iter().map(|l| l.to_string()));
        out.join("\n")
    }

    /// Comment on a part (optionally a specific face or vertex). Face, vertex and
    /// position are all optional; when all three are omitted and the human has
    /// clicked a spot in the live view, that pending pin is used instead. Annotations
    /// are not geometry: they never bump the model version but do persist in prefabs.
    pub fn add_annotation(
        &mut self,
        part: &str,
        text: &str,
        face: Option<u32>,
        vertex: Option<u32>,
        position: Option<[f32; 3]>,
    ) -> CmdResult {
        self.annotation_rev += 1;
        let (part, face, vertex, position) = if face.is_none()
            && vertex.is_none()
            && position.is_none()
            && let Some(pin) = self.pending_pin.take()
        {
            (pin.part, pin.face, pin.vertex, pin.position)
        } else {
            let centroid = self
                .model
                .part_centroid(part)
                .ok_or_else(|| ModelerError::NoPart(part.to_string()))?;
            (part.to_string(), face, vertex, position.unwrap_or(centroid))
        };
        let id = self.model.next_annotation_id;
        self.model.next_annotation_id += 1;
        let annotation = Annotation {
            id,
            part: part.clone(),
            face,
            vertex,
            position,
            text: text.to_string(),
            group: None,
            material: None,
            node: None,
            resolved: None,
        };
        self.model.annotations.push(annotation);
        Ok(format!(
            "annotation #{id} on `{part}`: '{text}'{}",
            face.map(|f| format!(" @face {f}")).unwrap_or_default()
        ))
    }

    /// Comment on an outliner group or a material (node). Group comments sit
    /// at the group's centre; material comments have no position.
    pub fn annotate(&mut self, target: &crate::AnnotationTarget, text: &str) -> CmdResult {
        self.annotation_rev += 1;
        use crate::AnnotationTarget as T;
        let (part, group, material, node, position) = match target {
            T::Part(p) => return self.add_annotation(p, text, None, None, None),
            T::Group { label, parts } => {
                let Some((lo, hi)) = self.model.bounds_of(parts) else {
                    return Err(format!("group `{label}`: none of its parts exist").into());
                };
                let c = (lo + hi) * 0.5;
                (
                    parts.first().cloned().unwrap_or_default(),
                    Some(label.clone()),
                    None,
                    None,
                    c.to_array(),
                )
            }
            T::Material { material, node } => {
                let doc = self.materials.get(Some(material))?;
                if let Some(n) = node
                    && !doc
                        .nodes
                        .iter()
                        .any(|v| v.get("name").and_then(|x| x.as_str()) == Some(n.as_str()))
                {
                    return Err(format!("material `{material}` has no node `{n}`").into());
                }
                (
                    String::new(),
                    None,
                    Some(material.clone()),
                    node.clone(),
                    [0.0; 3],
                )
            }
        };
        let id = self.model.next_annotation_id;
        self.model.next_annotation_id += 1;
        let a = Annotation {
            id,
            part,
            face: None,
            vertex: None,
            position,
            text: text.to_string(),
            group,
            material,
            node,
            resolved: None,
        };
        let msg = format!("annotation #{id} on {}: '{text}'", a.target());
        self.model.annotations.push(a);
        Ok(msg)
    }

    /// Every annotation, one line each, so the AI can read the human's
    /// comments: the open ones first (the work to do), then the resolved.
    pub fn list_annotations(&self) -> String {
        if self.model.annotations.is_empty() {
            return "no annotations yet".into();
        }
        let open: Vec<_> = self
            .model
            .annotations
            .iter()
            .filter(|a| a.is_open())
            .collect();
        let done: Vec<_> = self
            .model
            .annotations
            .iter()
            .filter(|a| !a.is_open())
            .collect();
        let mut out = format!("{} open annotation(s)", open.len());
        if !done.is_empty() {
            out += &format!(", {} resolved", done.len());
        }
        out += "\n";
        for a in &open {
            out += &format!("{}\n", a.line());
        }
        if !done.is_empty() {
            out += "resolved:\n";
            for a in &done {
                out += &format!("{}\n", a.line());
            }
        }
        if !open.is_empty() {
            out +=
                "Once a comment is dealt with: add_annotation {resolve: id, text: what you did}.\n";
        }
        out
    }

    /// Mark a comment resolved with a reply (`reopen`: open it again).
    pub fn resolve_annotation(&mut self, id: u32, reply: &str, reopen: bool) -> CmdResult {
        self.annotation_rev += 1;
        let by = self.client.clone();
        let a = self
            .model
            .annotations
            .iter_mut()
            .find(|a| a.id == id)
            .ok_or_else(|| ModelerError::Invalid(format!("no annotation #{id}")))?;
        if reopen {
            a.resolved = None;
            return Ok(format!("reopened annotation #{id} ({})", a.target()));
        }
        let reply = reply.trim();
        if reply.is_empty() {
            return Err(ModelerError::Invalid(
                "resolve: say in `text` what was done about the comment".into(),
            ));
        }
        a.resolved = Some(crate::Resolution {
            reply: reply.to_string(),
            by,
        });
        Ok(format!(
            "resolved annotation #{id} ({}): '{reply}'",
            a.target()
        ))
    }

    /// Remove one annotation by id.
    pub fn delete_annotation(&mut self, id: u32) -> CmdResult {
        self.annotation_rev += 1;
        let before = self.model.annotations.len();
        self.model.annotations.retain(|a| a.id != id);
        if self.model.annotations.len() == before {
            Err(format!("no annotation #{id}").into())
        } else {
            Ok(format!("deleted annotation #{id}"))
        }
    }

    /// A short overview for a freshly loaded model: counts, bounds, visible
    /// parts (folded), params, named moves/places and warnings.
    /// `scene_summary` / `ops` give the full listing.
    pub fn load_brief(&self) -> String {
        let bounds = self
            .model
            .bounds()
            .map(|(min, max)| format!("bounds {min:?}..{max:?}"))
            .unwrap_or_else(|| "empty".into());
        let visible: Vec<String> = self
            .model
            .parts
            .iter()
            .filter(|p| p.visible)
            .map(|p| p.name.clone())
            .collect();
        let hidden = self.model.parts.len() - visible.len();
        let mut s = format!(
            "model `{}` v{}: {} parts ({hidden} hidden), {} ops, {bounds}\n",
            self.model.name,
            self.model.version,
            self.model.parts.len(),
            self.model.ops.len()
        );
        let names = crate::compact_names(&visible);
        let shown = names.len().min(40);
        s += &format!(
            "visible: {}{}\n",
            names[..shown].join(", "),
            if names.len() > shown { ", ..." } else { "" }
        );
        if !self.model.params.is_empty() {
            let ps: Vec<String> = self
                .model
                .params
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            s += &format!("params: {}\n", ps.join(", "));
        }
        let moves: Vec<String> = self
            .model
            .ops
            .iter()
            .filter_map(|o| match o {
                Op::Move { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect();
        if !moves.is_empty() {
            s += &format!("named moves/places: {}\n", moves.join(", "));
        }
        if !self.model.annotations.is_empty() {
            let open = self
                .model
                .annotations
                .iter()
                .filter(|a| a.is_open())
                .count();
            s += &format!(
                "annotations: {open} open, {} resolved{}\n",
                self.model.annotations.len() - open,
                if open > 0 { " (list_annotations)" } else { "" }
            );
        }
        let lints = self.lints();
        if lints.is_empty() {
            s += "warnings: none\n";
        } else {
            s += &format!("warnings ({}):\n", lints.len());
            for l in &lints {
                s += &format!("- {l}\n");
            }
        }
        s += "(scene_summary lists every part, ops the full graph)\n";
        s
    }

    pub fn summary(&self) -> String {
        let bounds = self
            .model
            .bounds()
            .map(|(min, max)| format!("bounds {min:?}..{max:?}"))
            .unwrap_or_else(|| "empty".into());
        let mut s = format!(
            "model `{}` v{}: {} parts, {bounds}\n",
            self.model.name,
            self.model.version,
            self.model.parts.len()
        );
        if let Some(note) = crate::catalog::autosave_note() {
            s += &format!("autosave: {note}\n");
        }
        if !self.others.is_empty() {
            s += &format!(
                "other open documents (scene switch): {}\n",
                self.doc_names()[1..].join(", ")
            );
        }
        if !self.model.style.is_empty() {
            let st = &self.model.style;
            s += &format!(
                "style{}: {}{} colours, {} presets, {} numbers (style show for tokens)\n",
                if st.name.is_empty() {
                    String::new()
                } else {
                    format!(" `{}`", st.name)
                },
                if st.brief.is_empty() {
                    String::new()
                } else {
                    format!("\"{}\"; ", st.brief)
                },
                st.colors.len(),
                st.materials.len(),
                st.numbers.len()
            );
        }
        if !self.model.ops.is_empty() {
            s += &(graph::describe(&self.model.ops) + "\n");
        }
        if let Some(rig) = crate::anim::check::summary(&self.model) {
            s += &(rig + "\n");
        }
        for p in &self.model.parts {
            s += &format!(
                "- `{}`{}: {} verts {} tris, mat {:?}, at {:?}\n",
                p.name,
                if p.visible { "" } else { " (hidden)" },
                p.verts.len(),
                p.tri_count(),
                p.material.base_color,
                p.translation
            );
        }
        if self.model.annotations.is_empty() {
            s += "annotations: none\n";
        } else {
            let open = self
                .model
                .annotations
                .iter()
                .filter(|a| a.is_open())
                .count();
            s += &format!(
                "annotations ({open} open, {} resolved)\n",
                self.model.annotations.len() - open
            );
            // Open comments are the human's requests: list them in full.
            for a in self.model.annotations.iter().filter(|a| a.is_open()) {
                s += &format!("- {}\n", a.line());
            }
            if open > 0 {
                s += "  resolve each when done: add_annotation {resolve: id, text: what you did}\n";
            }
        }
        if self.pending_pin.is_some() {
            s += "a pin is waiting in the live view: use add_annotation without face/vertex/position to comment on it\n";
        }
        let lints = self.lints();
        if !lints.is_empty() {
            s += &format!("warnings ({}):\n", lints.len());
            for l in &lints {
                s += &format!("- {l}\n");
            }
        }
        s += "tip: `validate` checks manifoldness and winding per part\n";
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_with_part() -> Session {
        let mut s = Session::default();
        s.add_primitive(
            crate::PrimitiveKind::Box,
            [1.0, 1.0, 1.0],
            [0.0, 0.0, 0.0],
            "leg",
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s
    }

    #[test]
    fn an_agent_call_leaves_the_human_on_their_document() {
        let mut s = Session::default();
        s.window_scene("open", Some("agents_doc"))
            .unwrap_or_else(|e| panic!("{e}"));
        s.window_scene("open", Some("humans_doc"))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.viewer_doc.as_deref(), Some("humans_doc"));
        s.begin_call(Some("agent"), Some("agents_doc"), None);
        assert_eq!(s.model.name, "agents_doc");
        assert!(s.viewer_away());
        s.end_call(Some("agent"));
        assert_eq!(s.model.name, "humans_doc");
        assert!(!s.viewer_away());
        // The agent's next call is back on its own document.
        s.begin_call(Some("agent"), None, None);
        assert_eq!(s.model.name, "agents_doc");
        s.end_call(Some("agent"));
        assert_eq!(s.model.name, "humans_doc");
        // Two overlapping calls: back to the human's only after both, and
        // the window's switch mid-call waits for them.
        s.begin_call(Some("agent"), None, None);
        s.begin_call(Some("other"), Some("agents_doc"), None);
        s.window_scene("switch", Some("agents_doc"))
            .unwrap_or_else(|e| panic!("{e}"));
        s.end_call(Some("agent"));
        assert_eq!(s.model.name, "agents_doc");
        s.end_call(Some("other"));
        assert_eq!(s.viewer_doc.as_deref(), Some("agents_doc"));
        assert!(!s.viewer_away());
    }

    #[test]
    fn resolving_keeps_the_comment_with_a_reply() {
        let mut s = session_with_part();
        s.add_annotation("leg", "too thin", None, None, None)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(s.resolve_annotation(0, "  ", false).is_err());
        assert!(s.resolve_annotation(7, "x", false).is_err());
        s.resolve_annotation(0, "made it 4 cm", false)
            .unwrap_or_else(|e| panic!("{e}"));
        let a = &s.model.annotations[0];
        assert!(!a.is_open());
        assert!(
            s.list_annotations()
                .contains("0 open annotation(s), 1 resolved")
        );
        assert!(s.list_annotations().contains("'made it 4 cm'"));
        let json = serde_json::to_string(&s.model).unwrap_or_else(|e| panic!("{e}"));
        let back: crate::Model = serde_json::from_str(&json).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(back.annotations[0].resolved, a.resolved);
        s.resolve_annotation(0, "", true)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(s.model.annotations[0].is_open());
    }

    #[test]
    fn unknown_part_errs() {
        let mut s = session_with_part();
        assert!(s.add_annotation("nope", "hi", None, None, None).is_err());
    }

    #[test]
    fn ids_are_monotonic() {
        let mut s = session_with_part();
        s.add_annotation("leg", "a", None, None, None)
            .unwrap_or_else(|e| panic!("{e}"));
        s.add_annotation("leg", "b", None, None, None)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.model.annotations[0].id, 0);
        assert_eq!(s.model.annotations[1].id, 1);
        assert!(s.model.annotation_at(1).is_some());
        assert!(s.model.annotation_at(9).is_none());
    }

    #[test]
    fn list_delete_round_trip() {
        let mut s = session_with_part();
        assert_eq!(s.list_annotations(), "no annotations yet");
        s.add_annotation("leg", "make this thicker", Some(12), None, None)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            s.list_annotations()
                .contains("#0 `leg` 'make this thicker' @face 12")
        );
        s.delete_annotation(0).unwrap_or_else(|e| panic!("{e}"));
        assert!(s.delete_annotation(0).is_err());
        assert_eq!(s.list_annotations(), "no annotations yet");
    }

    #[test]
    fn pending_pin_is_auto_consumed() {
        let mut s = session_with_part();
        s.pending_pin = Some(PendingPin {
            part: "leg".into(),
            face: Some(3),
            vertex: Some(7),
            position: [0.5, 0.0, 0.5],
        });
        s.add_annotation("leg", "here", None, None, None)
            .unwrap_or_else(|e| panic!("{e}"));
        let a = &s.model.annotations[0];
        assert_eq!(a.face, Some(3));
        assert_eq!(a.vertex, Some(7));
        assert_eq!(a.position, [0.5, 0.0, 0.5]);
        assert!(s.pending_pin.is_none());
    }

    #[test]
    fn annotations_do_not_bump_version_but_persist_via_serde() {
        let mut s = session_with_part();
        let v = s.model.version;
        s.add_annotation("leg", "note", None, None, Some([0.0, 1.0, 0.0]))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.model.version, v);

        // An old prefab without annotations still loads.
        let old = serde_json::to_string(&Model::named("old")).unwrap_or_else(|e| panic!("{e}"));
        let parsed: Model = serde_json::from_str(&old).unwrap_or_else(|e| panic!("{e}"));
        assert!(parsed.annotations.is_empty());

        // New prefabs carry their annotations.
        let json = serde_json::to_string(&s.model).unwrap_or_else(|e| panic!("{e}"));
        let parsed: Model = serde_json::from_str(&json).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(parsed.annotations[0].text, "note");
    }

    #[test]
    fn summary_includes_annotation_text() {
        let mut s = session_with_part();
        s.add_annotation("leg", "wobbly", None, None, None)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(s.summary().contains("wobbly"));
    }

    fn undo(s: &mut Session) {
        s.undo().unwrap_or_else(|e| panic!("{e}"));
    }

    #[test]
    fn history_tree_undoes_and_redoes_linear_edits() {
        let mut s = session_with_part();
        s.rename_part("leg", "leg2")
            .unwrap_or_else(|e| panic!("{e}"));
        undo(&mut s);
        assert_eq!(s.model.parts[0].name, "leg");
        s.redo().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.model.parts[0].name, "leg2");
        assert!(s.history(None).contains('#'));
    }

    #[test]
    fn editing_after_undo_branches_instead_of_discarding() {
        let mut s = session_with_part();
        s.rename_part("leg", "b").unwrap_or_else(|e| panic!("{e}"));
        undo(&mut s);
        // A new edit while back at the older state: the rename stays reachable.
        s.rename_part("leg", "c").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.model.parts[0].name, "c");
        assert!(s.history(None).contains("`rename `leg` -> `b`"));
        // Jump to the abandoned branch's snapshot and the name comes back.
        let id = s
            .model
            .history
            .nodes
            .iter()
            .position(|n| n.label.contains("-> `b`"))
            .unwrap_or_else(|| panic!("branch snapshot missing"));
        s.jump(id).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.model.parts[0].name, "b");
    }

    #[test]
    fn procedural_snapshots_serialize_without_parts_but_with_the_tree() {
        use crate::History;
        let mut s = Session::default();
        s.op_primitive(
            "leg",
            crate::PrimitiveKind::Box,
            [1.0, 1.0, 1.0],
            [0.0, 0.5, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s.op_primitive(
            "ball",
            crate::PrimitiveKind::Sphere,
            [0.5, 0.0, 0.0],
            [0.0, 2.0, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let json = serde_json::to_string(&s.model.history).unwrap_or_else(|e| panic!("{e}"));
        assert!(json.contains("ball"), "{json}");
        let back: History = serde_json::from_str(&json).unwrap_or_else(|e| panic!("{e}"));
        // The first snapshot is a baked empty model; the second is procedural
        // and carries ops only, re-deriving both parts.
        assert!(back.nodes[0].model.parts.is_empty());
        let mut model = back.nodes[1].model.clone();
        assert!(model.parts.is_empty());
        assert!(model.refresh().is_ok());
        assert_eq!(model.parts.len(), 2);
    }

    #[test]
    fn undo_at_the_root_errs() {
        let mut s = Session::default();
        assert!(s.undo().is_err());
        assert!(s.redo().is_err());
        assert!(s.jump(42).is_err());
    }

    #[test]
    fn ops_derive_parts_and_tweak_in_place() {
        let mut s = Session::default();
        s.op_primitive(
            "stem",
            crate::PrimitiveKind::Cylinder,
            [0.05, 1.0, 0.0],
            [0.0, 0.5, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s.op_primitive(
            "petal",
            crate::PrimitiveKind::Sphere,
            [0.2, 0.0, 0.0],
            [0.0, 1.0, 0.3],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s.op_radial_array(
            "ring",
            "petal",
            6,
            [0.0, 1.0, 0.0],
            360.0,
            0.0,
            crate::Axis::Y,
            false,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(s.model.procedural());
        // stem + petal + 6 ring copies
        assert_eq!(s.model.parts.len(), 8);
        assert!(s.summary().contains("ops (3)"));

        // Re-issuing the radial op with another count is the tweak path.
        let v = s.model.version;
        s.op_radial_array(
            "ring",
            "petal",
            8,
            [0.0, 1.0, 0.0],
            360.0,
            0.0,
            crate::Axis::Y,
            false,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.model.parts.len(), 10);
        assert_eq!(s.model.ops.len(), 3);
        assert!(s.model.version > v);
    }

    #[test]
    fn bad_op_is_rejected_and_leaves_graph_intact() {
        let mut s = Session::default();
        s.op_primitive(
            "a",
            crate::PrimitiveKind::Box,
            [1.0, 1.0, 1.0],
            [0.0, 0.0, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let v = s.model.version;
        assert!(
            s.op_boolean("cut", crate::BoolOp::Difference, "a", "ghost")
                .is_err()
        );
        assert_eq!(s.model.ops.len(), 1);
        assert_eq!(s.model.version, v);
        assert_eq!(s.model.parts.len(), 1);
    }

    #[test]
    fn op_delete_rederives() {
        let mut s = Session::default();
        s.op_primitive(
            "a",
            crate::PrimitiveKind::Box,
            [1.0, 1.0, 1.0],
            [0.0, 0.0, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s.op_primitive(
            "b",
            crate::PrimitiveKind::Box,
            [1.0, 1.0, 1.0],
            [1.0, 0.0, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s.op_delete("a").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.model.parts.len(), 1);
        assert!(s.op_delete("a").is_err());
    }

    fn box_op(name: &str, translation: [f32; 3]) -> Op {
        Op::Primitive {
            name: name.into(),
            kind: crate::PrimitiveKind::Box,
            size: [1.0, 1.0, 1.0],
            translation,
            rotation: None,
            frame: None,
        }
    }

    use serde_json::json;

    fn ok<T, E: std::fmt::Display>(r: Result<T, E>) -> T {
        r.unwrap_or_else(|e| panic!("{e}"))
    }

    fn sty(v: serde_json::Value) -> crate::style::Style {
        let mut st = crate::style::Style::default();
        ok(st.merge(&v));
        st
    }

    fn base_color(s: &Session, part: &str) -> [f32; 4] {
        let Some(p) = s.model.part(part) else {
            panic!("no part {part}");
        };
        p.material.base_color
    }

    #[test]
    fn scenes_switch_and_prefab_instances_follow_their_doc() {
        let mut s = Session::default();
        ok(s.new_model("scn_crate"));
        ok(s.set_style(sty(json!({
            "colors": {"primary": "#ff0000"}
        }))));
        ok(s.apply_values(
            None,
            vec![
                json!({"type": "primitive", "name": "box", "kind": "box", "size": 0.5}),
                json!({"type": "material", "name": "box", "color": "@primary"}),
            ],
        ));
        ok(s.scene("open", Some("scn_map")));
        assert_eq!(s.model.name, "scn_map");
        ok(s.set_style(sty(json!({
            "colors": {"primary": "#0000ff"}
        }))));
        ok(s.apply_values(
            None,
            vec![
                json!({"type": "prefab", "name": "a", "prefab": "scn_crate"}),
                json!({"type": "prefab", "name": "b", "prefab": "scn_crate",
                       "translation": [1, 0, 0], "style": "scene"}),
                json!({"type": "prefab", "name": "c", "prefab": "scn_crate",
                       "translation": [2, 0, 0], "style": {"colors": {"primary": "#00ff00"}}}),
            ],
        ));
        let red = base_color(&s, "a.box");
        assert!(red[0] > 0.9 && red[2] < 0.1, "own style: {red:?}");
        let blue = base_color(&s, "b.box");
        assert!(blue[2] > 0.9 && blue[0] < 0.1, "scene style: {blue:?}");
        let green = base_color(&s, "c.box");
        assert!(green[1] > 0.9 && green[0] < 0.1, "patched: {green:?}");
        // Scene style change re-themes only the "scene" instance.
        ok(s.set_style(sty(json!({
            "colors": {"primary": "#ffffff"}
        }))));
        assert!(base_color(&s, "b.box")[1] > 0.9);
        assert!(base_color(&s, "a.box")[1] < 0.1);
        // Edit the crate in its own doc: a new part shows up in the map.
        ok(s.scene("switch", Some("scn_crate")));
        let crate_history = s.model.history.nodes.len();
        ok(s.apply_values(
            None,
            vec![json!({"type": "primitive", "name": "lid", "kind": "box",
                        "size": [0.5, 0.05, 0.5], "translation": [0, 0.27, 0]})],
        ));
        ok(s.scene("switch", Some("scn_map")));
        assert!(s.model.part("a.lid").is_some(), "instance synced");
        ok(s.scene("switch", Some("scn_crate")));
        assert_eq!(s.model.history.nodes.len(), crate_history + 1);
        assert!(ok(s.scene("list", None)).contains("instances scn_crate"));
        ok(s.scene("close", Some("scn_map")));
        assert_eq!(s.doc_names(), vec!["scn_crate".to_string()]);
        assert!(crate::catalog::open_doc("scn_map").is_none());
    }

    #[test]
    fn verify_sees_collisions_a_catch_all_allow_hid() {
        // The typewriter bug: a bar sunk 2 cm into the body, held up only by
        // that overlap, excused by a catch-all allow.
        let mut s = Session::default();
        ok(s.apply_values(
            None,
            vec![
                json!({"type": "primitive", "name": "body", "kind": "box", "size": [0.3, 0.1, 0.2],
                       "translation": [0, 0.05, 0]}),
                json!({"type": "primitive", "name": "bar", "kind": "box", "size": [0.0016, 0.0022, 0.05],
                       "translation": [0, 0.06, 0.11]}),
            ],
        ));
        let spec = |j: serde_json::Value| -> VerifySpec { ok(serde_json::from_value(j)) };
        // Depth is along the bar, not its 1.6 mm thickness.
        let o = crate::contact::overlaps(&s.model);
        let bar = o.iter().find(|o| o.involves("bar"));
        let depth = bar.map_or(0.0, |o| o.depth);
        assert!((depth - 0.015).abs() < 1e-4, "depth {depth}");
        // Catch-all allow passes but says what it excused.
        let (pass, text) = s.verify(&spec(json!({"collisions": {"allow": [["bar", "*"]]}})));
        assert!(pass, "{text}");
        assert!(
            text.contains("excused 1 pair") && text.contains("catch-all"),
            "{text}"
        );
        // A depth-limited allow fails it.
        let (pass, text) = s.verify(&spec(json!({"collisions": {"allow": [
            {"pair": ["bar", "body"], "depth": 0.002}]}})));
        assert!(!pass, "{text}");
        assert!(text.contains("15.0 mm deep"), "{text}");
        // Grounded passes but names the deep joint; strict fails it.
        let (pass, text) = s.verify(&spec(json!({"grounded": true})));
        assert!(
            pass && text.contains("passing deep") && text.contains("`bar`"),
            "{text}"
        );
        let (pass, text) = s.verify(&spec(json!({"grounded": "strict"})));
        assert!(!pass, "{text}");
        // Scope: only the cap's checks (it floats; the bar's collision drops out).
        ok(s.apply_values(
            None,
            vec![json!({"type": "primitive", "name": "cap", "kind": "box", "size": [0.02, 0.002, 0.02],
                        "translation": [0.1, 0.2, 0]})],
        ));
        let (pass, text) = s.verify(&spec(json!({"scope": ["cap"], "grounded": true,
            "collisions": true, "validate": true})));
        assert!(!pass, "{text}");
        assert!(text.contains("floating") && text.contains("cap"), "{text}");
        assert!(!text.contains("`bar`"), "{text}");
        assert!(text.contains("PASS collisions"), "{text}");
    }

    #[test]
    fn restyle_reaches_repeated_in_place_materials() {
        let mut s = Session::default();
        ok(s.set_style(sty(json!({"colors": {"primary": "#ff0000"}}))));
        let items = vec![
            json!({"type": "primitive", "name": "box", "kind": "box", "size": 0.5}),
            json!({"type": "material", "name": "box", "color": "@primary"}),
        ];
        ok(s.apply_values(None, items.clone()));
        ok(s.apply_values(None, items));
        ok(s.set_style(sty(json!({"colors": {"primary": "#0000ff"}}))));
        let c = base_color(&s, "box");
        assert!(c[2] > 0.9 && c[0] < 0.1, "{c:?}");
    }

    #[test]
    fn new_model_is_undoable() {
        let mut s = session_with_part();
        let old_name = s.model.name.clone();
        s.new_model("second").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.model.name, "second");
        assert!(s.model.parts.is_empty());
        // One undo brings the old model back intact.
        s.undo().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.model.name, old_name);
        assert_eq!(s.model.parts.len(), 1);
    }

    #[test]
    fn apply_ops_batch_applies_in_order() {
        let mut s = Session::default();
        let ops = vec![
            box_op("a", [0.0, 0.5, 0.0]),
            box_op("b", [3.0, 0.5, 0.0]),
            Op::Material {
                name: "a".into(),
                input: "a".into(),
                base_color: Some([1.0, 0.0, 0.0, 1.0]),
                roughness: None,
                metalness: None,
                pbr: None,
                flat: false,
            },
        ];
        let out = s.apply_ops(ops).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.model.parts.len(), 2);
        assert!(out.contains("material `a` <- `a`"));
        assert!(out.contains("model `model`"));
    }

    #[test]
    fn apply_ops_stops_at_first_error_and_keeps_earlier_ops() {
        let mut s = Session::default();
        let ops = vec![
            box_op("a", [0.0, 0.5, 0.0]),
            Op::Boolean {
                name: "c".into(),
                op: crate::BoolOp::Union,
                a: "a".into(),
                b: "ghost".into(),
            },
            box_op("b", [3.0, 0.5, 0.0]),
        ];
        let Err(err) = s.apply_ops(ops) else {
            panic!("expected the batch to fail")
        };
        assert!(
            err.to_string().contains("batch item #1"),
            "error names the failing index: {err}"
        );
        assert_eq!(s.model.parts.len(), 1, "op #0 stayed applied");
        assert!(!s.model.ops.iter().any(|o| crate::graph::output(o) == "b"));
    }

    #[test]
    fn hide_and_show_part() {
        let mut s = session_with_part();
        s.set_visible("leg", false)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(!s.model.parts[0].visible);
        assert!(s.summary().contains("(hidden)"));
        s.set_visible("leg", true).unwrap_or_else(|e| panic!("{e}"));
        assert!(s.model.parts[0].visible);
        assert!(s.set_visible("nope", false).is_err());
    }

    #[test]
    fn hidden_part_is_excluded_from_bounds() {
        let mut s = session_with_part();
        let Some((amin, amax)) = s.model.bounds() else {
            panic!("bounds should exist")
        };
        assert!((amin.y + 0.5).abs() < 1e-5 && (amax.y - 0.5).abs() < 1e-5);
        s.set_visible("leg", false)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(s.model.bounds().is_none());
    }

    #[test]
    fn op_get_returns_full_parameters() {
        let mut s = Session::default();
        s.op_primitive(
            "leg",
            crate::PrimitiveKind::Cylinder,
            [0.1, 0.5, 0.0],
            [0.2, 0.25, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let d = s.describe_op("leg").unwrap_or_else(|e| panic!("{e}"));
        assert!(d.contains("cylinder"), "{d}");
        assert!(d.contains("size=[0.1, 0.5, 0.0]"), "{d}");
        assert!(d.contains("at=[0.2, 0.25, 0.0]"), "{d}");
        assert!(s.describe_op("nope").is_err());
    }

    #[test]
    fn copies_inherit_a_material_placed_after_them() {
        let mut s = Session::default();
        s.op_primitive(
            "arm",
            crate::PrimitiveKind::Box,
            [0.1, 0.5, 0.1],
            [0.4, 1.0, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s.op_mirror("arm_r", "arm", crate::Axis::X, [0.0, 0.0, 0.0])
            .unwrap_or_else(|e| panic!("{e}"));
        s.op_linear_array("arms", "arm", 3, [0.0, 0.0, 1.0])
            .unwrap_or_else(|e| panic!("{e}"));
        // Material op written after the copies: lineage still colors them.
        s.op_material("arm", "arm", Some([1.0, 0.0, 0.0, 1.0]), None, None)
            .unwrap_or_else(|e| panic!("{e}"));
        for n in ["arm", "arm_r", "arms_1", "arms_2"] {
            let p = s.model.part(n).unwrap_or_else(|| panic!("{n}"));
            assert_eq!(p.material.base_color, [1.0, 0.0, 0.0, 1.0], "{n}");
        }
        assert!(!s.summary().contains("default material"));
    }

    #[test]
    fn lint_full_ring_radial_duplicate() {
        let mut s = Session::default();
        s.op_primitive(
            "post",
            crate::PrimitiveKind::Cylinder,
            [0.05, 0.5, 0.0],
            [0.6, 0.25, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s.op_radial_array(
            "posts",
            "post",
            6,
            [0.0, 0.25, 0.0],
            360.0,
            0.0,
            crate::Axis::Y,
            false,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            s.lints()
                .iter()
                .any(|l| l.contains("source and copy 0 overlap")),
            "lints: {:?}",
            s.lints()
        );
        // The even variant (count-1 copies starting one step in) is clean.
        let mut s2 = Session::default();
        s2.op_primitive(
            "post",
            crate::PrimitiveKind::Cylinder,
            [0.05, 0.5, 0.0],
            [0.6, 0.25, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s2.op_radial_array(
            "posts",
            "post",
            5,
            [0.0, 0.25, 0.0],
            360.0,
            60.0,
            crate::Axis::Y,
            false,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(!s2.lints().iter().any(|l| l.contains("overlap")));
    }

    #[test]
    fn lint_default_material_part() {
        let s = session_with_part();
        assert!(
            s.lints()
                .iter()
                .any(|l| l.contains("`leg` still has the default material")),
            "lints: {:?}",
            s.lints()
        );
    }

    #[test]
    fn lint_coincident_bounds_zfight() {
        let mut s = Session::default();
        let ops = vec![box_op("a", [0.0, 0.5, 0.0]), box_op("b", [0.0, 0.5, 0.0])];
        s.apply_ops(ops).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            s.lints().iter().any(|l| l.contains("identical bounds")),
            "lints: {:?}",
            s.lints()
        );
    }

    #[test]
    fn lint_boolean_volume_sanity() {
        // A difference whose output kept the input's full volume: the cutter
        // misses. Built by hand (ops + matching parts) so the volumes are
        // exact — a real difference with an overlapping cutter shrinks.
        let mut s = Session::default();
        let mut a = Part::new("a");
        a.verts = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 1.0],
            [1.0, 1.0, 1.0],
            [0.0, 1.0, 1.0],
        ];
        a.indices = vec![
            0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7, 0, 1, 5, 0, 5, 4, 1, 2, 6, 1, 6, 5, 2, 3, 7, 2, 7,
            6, 3, 0, 4, 3, 4, 7,
        ];
        crate::uv::ensure_normals(&mut a);
        let mut hollow = a.clone();
        hollow.name = "hollow".into();
        let mut cutter = Part::new("b");
        cutter.verts = vec![
            [0.1, 0.1, 0.1],
            [0.9, 0.1, 0.1],
            [0.9, 0.9, 0.1],
            [0.1, 0.9, 0.1],
            [0.1, 0.1, 0.9],
            [0.9, 0.1, 0.9],
            [0.9, 0.9, 0.9],
            [0.1, 0.9, 0.9],
        ];
        cutter.indices = a.indices.clone();
        crate::uv::ensure_normals(&mut cutter);
        s.model.parts = vec![a.into(), cutter.into(), hollow.into()];
        s.model.ops = vec![
            box_op("a", [0.0, 0.0, 0.0]),
            box_op("b", [0.0, 0.0, 0.0]),
            Op::Boolean {
                name: "hollow".into(),
                op: crate::BoolOp::Difference,
                a: "a".into(),
                b: "b".into(),
            },
        ];
        s.model.version += 1;
        let lints = s.lints();
        assert!(
            lints
                .iter()
                .any(|l| l.contains("difference `hollow` left `a` at its original volume")),
            "lints: {:?}",
            lints
        );

        // A union whose output volume exactly equals a + b: the inputs
        // never overlapped. Two disjoint slabs, hand-welded union mesh.
        let mut s2 = Session::default();
        let mut slab_a = Part::new("sa");
        slab_a.verts = vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 1.0],
            [1.0, 1.0, 1.0],
            [0.0, 1.0, 1.0],
        ];
        slab_a.indices = vec![
            0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7, 0, 1, 5, 0, 5, 4, 1, 2, 6, 1, 6, 5, 2, 3, 7, 2, 7,
            6, 3, 0, 4, 3, 4, 7,
        ];
        let mut slab_b = slab_a.clone();
        slab_b.name = "sb".into();
        for v in &mut slab_b.verts {
            v[0] += 5.0;
        }
        let mut joined = Part::new("joined");
        joined.verts = [slab_a.verts.as_slice(), slab_b.verts.as_slice()].concat();
        joined.indices = slab_a.indices.clone();
        let off = slab_a.verts.len() as u32;
        joined
            .indices
            .extend(slab_b.indices.iter().map(|i| i + off));
        crate::uv::ensure_normals(&mut slab_a);
        crate::uv::ensure_normals(&mut slab_b);
        crate::uv::ensure_normals(&mut joined);
        s2.model.parts = vec![slab_a.into(), slab_b.into(), joined.into()];
        s2.model.ops = vec![
            box_op("sa", [0.0, 0.5, 0.0]),
            box_op("sb", [5.5, 0.5, 0.0]),
            Op::Boolean {
                name: "joined".into(),
                op: crate::BoolOp::Union,
                a: "sa".into(),
                b: "sb".into(),
            },
        ];
        s2.model.version += 1;
        let lints = s2.lints();
        assert!(
            lints
                .iter()
                .any(|l| l.contains("the inputs have no overlap")),
            "lints: {:?}",
            lints
        );
        // Unioning disjoint cutters is intentional: once the union is cut
        // out of something, the no-overlap warning stays quiet.
        s2.model.ops.push(Op::Boolean {
            name: "cut".into(),
            op: crate::BoolOp::Difference,
            a: "sa".into(),
            b: "joined".into(),
        });
        assert!(
            !s2.lints().iter().any(|l| l.contains("no overlap")),
            "cutter union still warned"
        );
    }

    #[test]
    fn validate_flags_inverted_mesh_and_passes_primitives() {
        let mut s = Session::default();
        s.op_primitive(
            "box",
            crate::PrimitiveKind::Box,
            [1.0, 1.0, 1.0],
            [0.0, 0.5, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(s.validate().contains("all parts: manifold, outward-wound"));

        // An inward-wound closed tetrahedron flips the signed volume negative.
        let mut p = Part::new("tetra");
        p.verts = vec![
            [1.0, 1.0, 1.0],
            [-1.0, -1.0, 1.0],
            [-1.0, 1.0, -1.0],
            [1.0, -1.0, -1.0],
        ];
        p.indices = vec![0, 1, 2, 0, 2, 3, 0, 3, 1, 1, 3, 2];
        crate::uv::ensure_normals(&mut p);
        s.model.parts.push(p.into());
        s.model.version += 1;
        let v = s.validate();
        assert!(v.contains("`tetra`: inverted winding"), "{v}");
        assert!(!v.contains("`box`"), "the good part is not reported: {v}");
    }

    #[test]
    fn check_rebuild_detects_stale_cache() {
        let mut s = Session::default();
        s.op_primitive(
            "box",
            crate::PrimitiveKind::Box,
            [1.0, 1.0, 1.0],
            [0.0, 0.5, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s.check_rebuild().unwrap_or_else(|e| panic!("{e}"));
        // Simulate drift: nudge the cached part without touching the ops.
        std::sync::Arc::make_mut(&mut s.model.parts[0]).translation = [9.0, 0.5, 0.0];
        let Err(err) = s.check_rebuild() else {
            panic!("stale cache should be reported")
        };
        assert!(err.to_string().contains("stale"), "{err}");
    }

    #[test]
    fn op_primitive_rotation_is_absolute() {
        let mut s = Session::default();
        let h = std::f32::consts::FRAC_1_SQRT_2;
        let z90 = crate::quat_of([0.0, 0.0, h, h]).to_array();
        s.op_primitive(
            "beam",
            crate::PrimitiveKind::Box,
            [2.0, 0.2, 0.2],
            [0.0, 0.1, 0.0],
            Some(z90),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.model.parts[0].rotation, z90);
        // And the upsert path accepts a changed rotation.
        s.op_primitive(
            "beam",
            crate::PrimitiveKind::Box,
            [2.0, 0.2, 0.2],
            [0.0, 0.1, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.model.parts[0].rotation, [0.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn expect_parts_reports_missing_and_extra() {
        let mut s = Session::default();
        s.op_primitive(
            "a",
            crate::PrimitiveKind::Box,
            [1.0, 1.0, 1.0],
            [0.0, 0.5, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s.expect_parts(&["a".into()])
            .unwrap_or_else(|e| panic!("{e}"));
        let Err(err) = s.expect_parts(&["a".into(), "b".into()]) else {
            panic!("missing part should be reported")
        };
        assert!(err.to_string().contains("missing"), "{err}");
        let Err(err) = s.expect_parts(&[]) else {
            panic!("extra part should be reported")
        };
        assert!(err.to_string().contains("unexpected"), "{err}");
    }

    #[test]
    fn expect_bounds_checks_axes_with_tolerance() {
        let mut s = Session::default();
        s.op_primitive(
            "box",
            crate::PrimitiveKind::Box,
            [1.0, 1.0, 1.0],
            [0.0, 0.5, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        // Sits on the ground, 1 unit tall, centered in x (spans x -0.5..0.5).
        s.expect_bounds(
            [Some(-0.5), Some(0.0), None],
            [Some(0.5), Some(1.0), None],
            0.01,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        // Off-by-more-than-tolerance expectations fail, naming the axis:
        // demanding min y 0.5 on a ground-sitting box = "the model floats".
        let Err(err) = s.expect_bounds([None, Some(0.5), None], [None, Some(1.0), None], 0.01)
        else {
            panic!("floating model should be reported")
        };
        assert!(err.to_string().contains("min y"), "{err}");
        // Skipped axes (None) never fail.
        s.expect_bounds([None, None, None], [None, None, None], 0.01)
            .unwrap_or_else(|e| panic!("{e}"));
    }

    #[test]
    fn colliding_output_names_are_rejected() {
        let mut s = Session::default();
        s.op_primitive("p", crate::PrimitiveKind::Box, [1.0; 3], [0.0; 3], None)
            .unwrap_or_else(|e| panic!("{e}"));
        s.op_radial_array("ring", "p", 3, [0.0; 3], 360.0, 0.0, crate::Axis::Y, false)
            .unwrap_or_else(|e| panic!("{e}"));
        let err = s.op_primitive(
            "ring_0",
            crate::PrimitiveKind::Box,
            [1.0; 3],
            [0.0; 3],
            None,
        );
        assert!(err.is_err(), "ring_0 is already a radial output");
        assert_eq!(s.model.ops.len(), 2);
    }

    #[test]
    fn op_delete_reports_array_dependents() {
        let mut s = Session::default();
        s.op_primitive("p", crate::PrimitiveKind::Box, [1.0; 3], [0.0; 3], None)
            .unwrap_or_else(|e| panic!("{e}"));
        s.op_radial_array("ring", "p", 3, [0.0; 3], 360.0, 0.0, crate::Axis::Y, false)
            .unwrap_or_else(|e| panic!("{e}"));
        s.op_transform("moved", "ring_1", Some([0.0, 3.0, 0.0]), None, None)
            .unwrap_or_else(|e| panic!("{e}"));
        let err = s
            .op_delete("ring")
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(err.to_string().contains("moved"), "{err}");
        assert_eq!(s.model.ops.len(), 3);
    }

    #[test]
    fn undo_keeps_baked_hide_on_op_models() {
        let mut s = Session::default();
        s.op_primitive("p", crate::PrimitiveKind::Box, [1.0; 3], [0.0; 3], None)
            .unwrap_or_else(|e| panic!("{e}"));
        s.set_visible("p", false).unwrap_or_else(|e| panic!("{e}"));
        s.op_primitive(
            "q",
            crate::PrimitiveKind::Box,
            [1.0; 3],
            [3.0, 0.0, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s.undo().unwrap_or_else(|e| panic!("{e}"));
        assert!(
            s.model.part("p").is_some_and(|p| !p.visible),
            "hide lost on undo"
        );
    }

    fn op_session() -> Session {
        let mut s = Session::default();
        s.op_primitive("p", crate::PrimitiveKind::Box, [1.0; 3], [0.0; 3], None)
            .unwrap_or_else(|e| panic!("{e}"));
        s.op_radial_array("ring", "p", 3, [0.0; 3], 360.0, 0.0, crate::Axis::Y, false)
            .unwrap_or_else(|e| panic!("{e}"));
        s
    }

    #[test]
    fn hide_on_op_models_is_a_graph_op_that_survives_tweaks() {
        let mut s = op_session();
        s.set_visible("p", false).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            s.model
                .ops
                .iter()
                .any(|o| matches!(o, Op::Visibility { .. }))
        );
        // Tweaking an upstream op re-derives, but the hide is part of the graph.
        s.op_primitive("p", crate::PrimitiveKind::Box, [2.0; 3], [0.0; 3], None)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(s.model.part("p").is_some_and(|p| !p.visible));
        // Copies made from the hidden template are visible.
        assert!(s.model.part("ring_1").is_some_and(|p| p.visible));
        // Re-issuing replaces (no stacking), and show works.
        s.set_visible("p", true).unwrap_or_else(|e| panic!("{e}"));
        s.set_visible("p", false).unwrap_or_else(|e| panic!("{e}"));
        s.set_visible("p", true).unwrap_or_else(|e| panic!("{e}"));
        let n = s
            .model
            .ops
            .iter()
            .filter(|o| matches!(o, Op::Visibility { .. }))
            .count();
        assert_eq!(n, 1);
        assert!(s.model.part("p").is_some_and(|p| p.visible));
    }

    #[test]
    fn rename_on_op_models_survives_tweaks_and_keeps_readers_working() {
        let mut s = op_session();
        s.rename_part("p", "seed").unwrap_or_else(|e| panic!("{e}"));
        assert!(s.model.part("seed").is_some());
        assert!(s.model.part("p").is_none());
        // The radial op still reads the old name and runs before the rename.
        assert!(s.model.part("ring_0").is_some());
        s.op_primitive("p", crate::PrimitiveKind::Box, [3.0; 3], [0.0; 3], None)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(s.model.part("seed").is_some(), "rename lost after a tweak");
        s.check_rebuild().unwrap_or_else(|e| panic!("{e}"));
        // Undo returns to the pre-rename names.
        s.undo().unwrap_or_else(|e| panic!("{e}"));
        s.undo().unwrap_or_else(|e| panic!("{e}"));
        assert!(s.model.part("p").is_some());
    }

    #[test]
    fn material_and_transform_on_op_models_are_graph_ops() {
        let mut s = op_session();
        s.set_material("p", Some([0.0, 1.0, 0.0, 1.0]), None, None)
            .unwrap_or_else(|e| panic!("{e}"));
        s.transform_part("p", Some([0.0, 5.0, 0.0]), None, None)
            .unwrap_or_else(|e| panic!("{e}"));
        s.op_primitive("p", crate::PrimitiveKind::Box, [2.0; 3], [0.0; 3], None)
            .unwrap_or_else(|e| panic!("{e}"));
        let p = s.model.part("p").unwrap_or_else(|| panic!("no p"));
        assert_eq!(p.material.base_color, [0.0, 1.0, 0.0, 1.0]);
        assert_eq!(p.translation, [0.0, 5.0, 0.0]);
    }

    #[test]
    fn op_snapshots_store_no_parts_and_undo_rederives() {
        let mut s = op_session();
        s.op_primitive(
            "q",
            crate::PrimitiveKind::Box,
            [1.0; 3],
            [4.0, 0.0, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            s.model
                .history
                .nodes
                .iter()
                .all(|n| n.model.parts.is_empty())
        );
        let live = s.model.parts.len();
        s.undo().unwrap_or_else(|e| panic!("{e}"));
        assert!(s.model.part("q").is_none());
        assert!(!s.model.parts.is_empty());
        s.redo().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.model.parts.len(), live);
    }

    #[test]
    fn history_prune_keeps_newest_branch_points_and_cursor() {
        let mut s = op_session();
        for i in 0..10 {
            s.op_primitive(
                "q",
                crate::PrimitiveKind::Box,
                [1.0 + i as f32; 3],
                [0.0; 3],
                None,
            )
            .unwrap_or_else(|e| panic!("{e}"));
        }
        let total = s.model.history.nodes.len();
        let out = s.history_prune(3).unwrap_or_else(|e| panic!("{e}"));
        let h = &s.model.history;
        assert!(h.nodes.len() < total, "{out}");
        assert_eq!(h.current, Some(h.nodes.len() - 1));
        // Tree stays consistent: every parent link has the matching child link.
        for (i, n) in h.nodes.iter().enumerate() {
            if let Some(p) = n.parent {
                assert!(h.nodes[p].children.contains(&i));
            }
        }
        // Still navigable.
        s.undo().unwrap_or_else(|e| panic!("{e}"));
        s.redo().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.history_prune(1000).map(|_| ()), Ok(()));
    }

    #[test]
    fn errors_are_typed_so_tests_can_match_variants() {
        use crate::ModelerError;
        let mut s = op_session();
        assert_eq!(
            s.set_uvs("ghost", "box"),
            Err(ModelerError::NoPart("ghost".into()))
        );
        // A dangling input surfaces as MissingInput, not a stringly error.
        let err = s.op_transform("t", "ghost", None, None, None).err();
        assert_eq!(err, Some(ModelerError::MissingInput("ghost".into())));
        assert!(matches!(
            crate::graph::evaluate(&[
                Op::Transform {
                    name: "a".into(),
                    input: "b".into(),
                    translation: None,
                    rotation: None,
                    scale: None,
                    relative: false,
                    pivot: None,
                },
                Op::Transform {
                    name: "b".into(),
                    input: "a".into(),
                    translation: None,
                    rotation: None,
                    scale: None,
                    relative: false,
                    pivot: None,
                },
            ]),
            Err(ModelerError::Cycle(_))
        ));
    }

    #[test]
    fn baked_history_snapshots_share_unchanged_parts() {
        let mut s = session_with_part();
        s.add_primitive(
            crate::PrimitiveKind::Box,
            [1.0; 3],
            [5.0, 0.0, 0.0],
            "other",
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s.transform_part("other", Some([6.0, 0.0, 0.0]), None, None)
            .unwrap_or_else(|e| panic!("{e}"));
        let nodes = &s.model.history.nodes;
        let (a, b) = (&nodes[nodes.len() - 2].model, &nodes[nodes.len() - 1].model);
        // `leg` was untouched by the last edit: both snapshots point at one mesh.
        assert!(std::sync::Arc::ptr_eq(&a.parts[0], &b.parts[0]));
        assert!(!std::sync::Arc::ptr_eq(&a.parts[1], &b.parts[1]));
    }

    #[test]
    fn radial_include_source_spaces_count_items_evenly_without_overlap() {
        let mut s = Session::default();
        s.op_primitive(
            "blade",
            crate::PrimitiveKind::Box,
            [0.1, 1.0, 0.1],
            [0.0, 1.0, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s.op_radial_array(
            "blades",
            "blade",
            4,
            [0.0; 3],
            360.0,
            0.0,
            crate::Axis::Z,
            true,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        // Source + 3 copies at 90/180/270; no `blades_0` overlapping the source.
        assert!(s.model.part("blades_0").is_none());
        assert!(s.model.part("blades_3").is_some());
        let x = s
            .model
            .part("blades_1")
            .map(|p| p.translation[0])
            .unwrap_or(9.0);
        assert!(
            (x + 1.0).abs() < 1e-5,
            "90 degrees about z moves +y to -x, got {x}"
        );
        assert!(
            s.lints().iter().all(|l| !l.contains("overlap")),
            "{:?}",
            s.lints()
        );
    }

    #[test]
    fn expect_bounds_names_the_part_that_sets_the_extreme() {
        let mut s = op_session();
        let err = s
            .expect_bounds([None; 3], [None, Some(0.0), None], 0.01)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(err.contains("set by `"), "{err}");
        let _ = &mut s;
    }

    #[test]
    fn boolean_outputs_are_closed_and_lint_clean() {
        let mut s = Session::default();
        s.op_primitive(
            "a",
            crate::PrimitiveKind::Cylinder,
            [1.0, 2.0, 0.0],
            [0.0; 3],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s.op_primitive(
            "b",
            crate::PrimitiveKind::Box,
            [0.5, 0.5, 3.0],
            [0.4, 0.0, 0.0],
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s.op_boolean("c", crate::BoolOp::Difference, "a", "b")
            .unwrap_or_else(|e| panic!("{e}"));
        let part = s.model.part("c").unwrap_or_else(|| panic!("c"));
        // Manifold outputs are closed, so the open-edge lint stays silent.
        assert_eq!(crate::verify::open_edge_count(part), 0);
        assert!(!s.lints().iter().any(|l| l.contains("open edges")));
    }

    #[test]
    fn params_rederive_dependent_ops() {
        use serde_json::json;
        let mut s = Session::default();
        s.apply_values(
            Some([("h".to_string(), 2.0)].into()),
            vec![
                json!({"type": "primitive", "name": "tower", "kind": "cylinder",
                       "size": [0.5, "$h"], "translation": [0, "=$h/2", 0]}),
                json!({"type": "primitive", "name": "roof", "kind": "cone",
                       "size": [0.6, 0.5], "translation": [0, "=$h + 0.25", 0]}),
            ],
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let top = |s: &Session| s.model.bounds().map(|(_, max)| max.y).unwrap_or(0.0);
        assert!((top(&s) - 2.5).abs() < 1e-4, "{}", top(&s));
        // One parameter change moves both ops.
        let out = s
            .apply_values(Some([("h".to_string(), 3.0)].into()), vec![])
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(out.contains("re-derived 2 op(s)"), "{out}");
        assert!((top(&s) - 3.5).abs() < 1e-4, "{}", top(&s));
        let tower = s.model.part("tower").unwrap_or_else(|| panic!("tower"));
        assert!((tower.translation[1] - 1.5).abs() < 1e-5);
        // Undo restores the old parameter with its geometry.
        s.undo().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.model.params.get("h"), Some(&2.0));
        // Unknown parameter: clear error, nothing applied.
        let Err(e) = s.apply_values(
            None,
            vec![json!({"type": "primitive", "name": "x", "kind": "box", "size": "$nope"})],
        ) else {
            panic!("unknown param should fail");
        };
        assert!(e.to_string().contains("unknown parameter `$nope`"), "{e}");
        assert!(s.model.part("x").is_none());
    }

    #[test]
    fn deform_reads_params_and_rederives() {
        use serde_json::json;
        let mut s = Session::default();
        s.apply_values(
            Some([("bend".to_string(), 30.0)].into()),
            vec![
                json!({"type": "primitive", "name": "horn_raw", "kind": "cylinder",
                       "size": [0.05, 0.6], "translation": [0, 0.3, 0]}),
                json!({"type": "deform", "name": "horn_t", "input": "horn_raw",
                       "mode": "taper", "scale_end": 0.1}),
                json!({"type": "deform", "name": "horn", "input": "horn_t",
                       "mode": "bend", "degrees": "$bend", "toward": "-z"}),
                json!({"type": "hide_part", "part": "horn_*"}),
            ],
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let depth = |s: &Session| {
            let (lo, _) = s
                .model
                .part("horn")
                .unwrap_or_else(|| panic!("horn"))
                .bounds();
            -lo.z
        };
        let before = depth(&s);
        let out = s
            .apply_values(Some([("bend".to_string(), 120.0)].into()), vec![])
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(out.contains("re-derived 1 op(s)"), "{out}");
        assert!(depth(&s) > before + 0.1, "{before} -> {}", depth(&s));
        let horn = s.model.part("horn").unwrap_or_else(|| panic!("horn"));
        assert_eq!(crate::verify::open_edge_count(horn), 0);
        let Err(e) = s.apply_values(
            None,
            vec![json!({"type": "deform", "name": "x", "input": "horn", "mode": "melt"})],
        ) else {
            panic!("bad mode should fail");
        };
        assert!(e.to_string().contains("mode"), "{e}");
    }

    #[test]
    fn diff_and_cherry_pick_recover_a_branch() {
        use serde_json::json;
        let mut s = Session::default();
        s.apply_values(
            None,
            vec![json!({"type": "primitive", "name": "a", "kind": "box", "size": 1})],
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let base = s.model.history.current.unwrap_or_else(|| panic!("cursor"));
        s.apply_values(
            None,
            vec![
                json!({"type": "primitive", "name": "b", "kind": "sphere", "size": 0.5, "translation": [2, 0, 0]}),
                json!({"type": "primitive", "name": "a", "kind": "box", "size": 2}),
            ],
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let tip = s.model.history.current.unwrap_or_else(|| panic!("cursor"));
        let d = s.diff(base, None).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            d.contains("+ primitive `b`") && d.contains("~ primitive `a`"),
            "{d}"
        );
        // Jump back, branch, then pick only `b` from the old tip.
        s.jump(base).unwrap_or_else(|e| panic!("{e}"));
        assert!(s.model.part("b").is_none());
        s.cherry_pick(tip, Some(vec!["b".into()]))
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(s.model.part("b").is_some());
        let a = s.model.part("a").unwrap_or_else(|| panic!("a"));
        assert!(
            (a.bounds().1.x - 0.5).abs() < 1e-5,
            "`a` must keep its branch size"
        );
        assert!(s.cherry_pick(tip, Some(vec!["zzz".into()])).is_err());
    }

    #[test]
    fn param_template_survives_an_in_place_material() {
        use serde_json::json;
        let mut s = Session::default();
        s.apply_values(
            Some([("h".to_string(), 2.0)].into()),
            vec![
                json!({"type": "primitive", "name": "t", "kind": "cylinder", "size": [0.5, "$h"]}),
                json!({"type": "material", "name": "t", "color": [1, 0, 0]}),
            ],
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s.apply_values(Some([("h".to_string(), 4.0)].into()), vec![])
            .unwrap_or_else(|e| panic!("{e}"));
        let t = s.model.part("t").unwrap_or_else(|| panic!("t"));
        assert!((t.bounds().1.y - 2.0).abs() < 1e-5, "height must follow h");
        assert_eq!(t.material.base_color, [1.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn an_apply_ops_call_is_one_undo_step() {
        use serde_json::json;
        let mut s = Session::default();
        s.new_model("t").unwrap_or_else(|e| panic!("{e}"));
        let before = s.model.history.nodes.len();
        s.apply_values(
            Some([("h".to_string(), 1.0)].into()),
            vec![
                json!({"type": "primitive", "name": "a", "kind": "box", "size": "$h"}),
                json!({"type": "primitive", "name": "b", "kind": "box", "size": 1, "translation": [3, 0, 0]}),
                json!({"type": "material", "name": "a", "color": "#f00"}),
            ],
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            s.model.history.nodes.len(),
            before + 1,
            "{}",
            s.history(None)
        );
        s.undo().unwrap_or_else(|e| panic!("{e}"));
        assert!(s.model.parts.is_empty(), "one undo reverts the whole call");
    }

    #[test]
    fn cherry_pick_brings_the_params_its_ops_were_built_with() {
        use serde_json::json;
        let mut s = Session::default();
        s.apply_values(
            Some([("h".to_string(), 1.0)].into()),
            vec![json!({"type": "primitive", "name": "a", "kind": "box", "size": [1, "$h", 1]})],
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let base = s
            .model
            .history
            .current
            .unwrap_or_else(|| panic!("no cursor"));
        s.apply_values(Some([("h".to_string(), 3.0)].into()), vec![])
            .unwrap_or_else(|e| panic!("{e}"));
        let tip = s
            .model
            .history
            .current
            .unwrap_or_else(|| panic!("no cursor"));
        s.jump(base).unwrap_or_else(|e| panic!("{e}"));
        s.apply_values(
            None,
            vec![json!({"type": "primitive", "name": "z", "kind": "box", "size": 1, "translation": [5, 0, 0]})],
        )
        .unwrap_or_else(|e| panic!("{e}"));
        // Picking the param by name works, and the op re-derives with it.
        s.cherry_pick(tip, Some(vec!["h".into()]))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.model.params.get("h"), Some(&3.0));
        let a = s.model.part("a").unwrap_or_else(|| panic!("no a"));
        let (lo, hi) = a.bounds();
        assert!((hi.y - lo.y - 3.0).abs() < 1e-5, "{lo} {hi}");
    }

    #[test]
    fn group_merges_disjoint_parts_and_arrays_as_one() {
        use serde_json::json;
        let mut s = Session::default();
        s.apply_values(
            None,
            vec![
                json!({"type": "primitive", "name": "t", "kind": "box", "size": 0.5}),
                json!({"type": "linear_array", "name": "t", "input": "t", "count": 3, "offset": [1, 0, 0]}),
                json!({"type": "group", "name": "row", "inputs": ["t", "t_1", "t_2"]}),
                json!({"type": "linear_array", "name": "rows", "input": "row", "count": 2, "offset": [0, 0, 1]}),
                json!({"type": "hide_part", "part": "t"}),
                json!({"type": "hide_part", "part": "t_1"}),
                json!({"type": "hide_part", "part": "t_2"}),
            ],
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let row = s.model.part("row").unwrap_or_else(|| panic!("no row"));
        assert_eq!(
            row.verts.len(),
            3 * s.model.part("t").map_or(0, |p| p.verts.len())
        );
        assert!(s.model.part("rows_1").is_some());
        let (_, hi) = s
            .model
            .part("rows_1")
            .unwrap_or_else(|| panic!("x"))
            .bounds();
        assert!(
            (hi.x - 2.25).abs() < 1e-5 && (hi.z - 1.25).abs() < 1e-5,
            "{hi}"
        );
        assert!(
            s.lints().iter().all(|l| !l.contains("no overlap")),
            "{:?}",
            s.lints()
        );
        let spec: VerifySpec = serde_json::from_value(
            json!({"parts": ["row", "rows_*"], "includes": ["rows_1"], "grounded": true, "validate": true}),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let checks = s.verify_checks(&spec);
        assert!(checks.iter().all(|c| c.1), "{checks:?}");
    }

    #[test]
    fn a_part_hanging_in_the_air_is_flagged() {
        use serde_json::json;
        let mut s = Session::default();
        s.apply_values(
            None,
            vec![
                json!({"type": "primitive", "name": "pole", "kind": "cylinder", "size": [0.03, 2], "translation": [0, 1, 0]}),
                json!({"type": "primitive", "name": "cap", "kind": "box", "size": 0.1, "translation": [0, 2.05, 0]}),
                json!({"type": "transform", "name": "stray", "input": "pole", "translation": [0.5, 3, 0]}),
            ],
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let lints = s.lints();
        assert!(
            lints.iter().any(|l| l.contains("`stray` floats")),
            "{lints:?}"
        );
        assert!(
            !lints.iter().any(|l| l.contains("`cap` floats")),
            "{lints:?}"
        );
    }

    #[test]
    fn reissuing_an_array_named_after_its_source_replaces_it() {
        use serde_json::json;
        let mut s = Session::default();
        let batch = vec![
            json!({"type": "primitive", "name": "tile", "kind": "box", "size": 0.2}),
            json!({"type": "linear_array", "name": "tile", "input": "tile", "count": 3, "offset": [0.3, 0, 0]}),
        ];
        s.apply_values(None, batch.clone())
            .unwrap_or_else(|e| panic!("{e}"));
        s.apply_values(None, batch)
            .unwrap_or_else(|e| panic!("re-issue failed: {e}"));
        assert_eq!(s.model.ops.len(), 2, "{}", s.describe_ops());
    }

    #[test]
    fn brief_apply_result_lists_only_what_changed() {
        let mut s = Session::default();
        s.new_model("t").unwrap_or_else(|e| panic!("{e}"));
        let ops = |v: serde_json::Value| -> Vec<serde_json::Value> {
            serde_json::from_value(v).unwrap_or_else(|e| panic!("{e}"))
        };
        let t = s
            .apply_values_as(
                None,
                ops(serde_json::json!([
                    {"type": "primitive", "name": "a", "kind": "box", "size": 1, "translation": [0, 0.5, 0]},
                    {"type": "primitive", "name": "b", "kind": "box", "size": 1, "translation": [3, 0.5, 0]}
                ])),
                false,
            )
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(t.contains("parts (2 changed): +a, +b"), "{t}");
        assert!(!t.contains("ops ("), "{t}");
        let t = s
            .apply_values_as(
                None,
                ops(serde_json::json!([
                    {"type": "primitive", "name": "b", "kind": "box", "size": 2, "translation": [3, 1, 0]}
                ])),
                false,
            )
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(t.contains("parts (1 changed): ~b"), "{t}");
        let spec = VerifySpec {
            stable: Some(true),
            ..Default::default()
        };
        assert!(s.verify(&spec).0);
    }

    #[test]
    fn delete_takes_own_edits_and_hide_takes_globs() {
        let mut s = Session::default();
        s.new_model("t").unwrap_or_else(|e| panic!("{e}"));
        let ops: Vec<serde_json::Value> = serde_json::from_value(serde_json::json!([
            {"type": "primitive", "name": "cut", "kind": "box", "size": 1},
            {"type": "linear_array", "name": "cut", "input": "cut", "count": 3, "offset": [2, 0, 0]},
            {"type": "hide_part", "part": "cut*"},
            {"type": "material", "name": "cut", "color": "#f00"}
        ]))
        .unwrap_or_else(|e| panic!("{e}"));
        let t = s
            .apply_values_as(None, ops, false)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(t.contains("3 part(s) hidden by `cut*`"), "{t}");
        assert!(s.model.parts.iter().all(|p| !p.visible));
        let ops: Vec<serde_json::Value> =
            serde_json::from_value(serde_json::json!([{"type": "delete", "name": "cut_2"}]))
                .unwrap_or_else(|e| panic!("{e}"));
        // cut_2 is an array output, not an op; deleting the source op
        // with its own hide/material edits must work.
        let _ = s.apply_values(None, ops);
        let ops: Vec<serde_json::Value> = serde_json::from_value(serde_json::json!([
            {"type": "delete", "name": "cut"}
        ]))
        .unwrap_or_else(|e| panic!("{e}"));
        let r = s.apply_values(None, ops);
        assert!(r.is_err(), "the array still reads `cut`");
    }

    fn run(s: &mut Session, items: serde_json::Value) -> String {
        let serde_json::Value::Array(items) = items else {
            panic!("array");
        };
        s.apply_values_as(None, items, true)
            .unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn place_drops_parts_onto_a_surface_and_follows_params() {
        let mut s = Session::default();
        s.apply_values_as(
            Some([("h".to_string(), 0.75)].into_iter().collect()),
            vec![
                serde_json::json!({"type": "primitive", "name": "top", "kind": "box", "size": [1.2, 0.03, 0.6], "translation": [0, "=$h - 0.015", 0]}),
                serde_json::json!({"type": "primitive", "name": "mug", "kind": "cylinder", "size": {"radius": 0.04, "height": 0.1}, "translation": [0, 3, 0]}),
                serde_json::json!({"type": "place", "name": "mug_on", "parts": ["mug"], "on": "top", "at": [0.3, 0.1]}),
            ],
            true,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let lo = |s: &Session, n: &str| s.model.part(n).map(|p| p.bounds().0).unwrap_or_default();
        let m = lo(&s, "mug");
        assert!(
            (m.y - 0.75).abs() < 1e-4 && (m.x - 0.26).abs() < 1e-4,
            "{m:?}"
        );
        // The desk gets lower: the mug follows.
        s.apply_values_as(
            Some([("h".to_string(), 0.7)].into_iter().collect()),
            vec![],
            true,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert!((lo(&s, "mug").y - 0.7).abs() < 1e-4);
        // A tilted support: lands on the slope, touching.
        run(
            &mut s,
            serde_json::json!([
                {"type": "primitive", "name": "ramp", "kind": "box", "size": [1, 0.05, 1], "translation": [3, 0.5, 0], "rotation": [0, 0, 0.1305, 0.9914]},
                {"type": "primitive", "name": "puck", "kind": "cylinder", "size": {"radius": 0.05, "height": 0.02}, "translation": [3, 2, 0]},
                {"type": "place", "name": "puck_on", "parts": ["puck"], "on": "ramp"}
            ]),
        );
        assert!(crate::contact::rests_on(&s.model, "puck", "ramp").is_ok());
        let bad = s.apply_values_as(
            None,
            vec![serde_json::json!({"type": "place", "parts": ["puck"], "on": "nope"})],
            true,
        );
        assert!(bad.is_err());
    }

    #[test]
    fn framed_primitives_and_local_arrays_follow_a_retilted_part() {
        let mut s = Session::default();
        let items = vec![
            serde_json::json!({"type": "primitive", "name": "paper", "kind": "box", "size": [0.2, 0.3, 0.002], "translation": [0.1, 0.5, 0], "rotation": {"x": "$tilt"}}),
            serde_json::json!({"type": "primitive", "name": "line", "kind": "box", "size": [0.15, 0.004, 0.001], "frame": "paper:top-front", "translation": [0, -0.02, 0.0005]}),
            serde_json::json!({"type": "linear_array", "name": "line", "input": "line", "count": 5, "offset": [0, -0.02, 0], "local": true}),
        ];
        let check = |s: &Session, tilt: f32| {
            let r = bevy::math::Quat::from_rotation_x(tilt.to_radians());
            let want = Vec3::new(0.1, 0.5, 0.0) + r * Vec3::new(0.0, 0.15 - 0.02 - 0.08, 0.0015);
            let Some(p) = s.model.part("line_4") else {
                panic!("no line_4");
            };
            let got = Vec3::from(p.translation);
            assert!(
                (got - want).length() < 1e-5,
                "tilt {tilt}: {got:?} vs {want:?}"
            );
            let q = crate::quat_of(p.rotation);
            assert!(
                q.dot(r).abs() > 1.0 - 1e-5,
                "line rotation follows the paper: {q:?} vs {r:?}"
            );
        };
        s.apply_values_as(
            Some([("tilt".to_string(), -20.0)].into_iter().collect()),
            items,
            true,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        check(&s, -20.0);
        s.apply_values_as(
            Some([("tilt".to_string(), -40.0)].into_iter().collect()),
            vec![],
            true,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        check(&s, -40.0);
        let bad = s.apply_values_as(
            None,
            vec![serde_json::json!({"type": "primitive", "name": "x", "kind": "box", "size": 0.1, "frame": "paper:upper"})],
            true,
        );
        assert!(bad.is_err_and(|e| e.to_string().contains("upper")));
    }

    #[test]
    fn repeat_expands_nested_loops_and_follows_params() {
        let mut s = Session::default();
        s.apply_values_as(
            Some([("h".to_string(), 0.1)].into_iter().collect()),
            vec![
                serde_json::json!({"type": "repeat", "var": "row", "values": [0, 1, 2], "items": [
                    {"type": "repeat", "var": "k", "count": "=4 - $row", "items": [
                        {"type": "primitive", "name": "key{row}_{k}", "kind": "box", "size": 0.01,
                         "translation": ["=$k * 0.02", "=$h + $row * 0.0025", "$row"]}
                    ]}
                ]}),
            ],
            true,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let at = |s: &Session, n: &str| {
            s.model
                .part(n)
                .map(|p| Vec3::from(p.translation))
                .unwrap_or_else(|| panic!("no {n}"))
        };
        assert!(s.model.part("key0_3").is_some() && s.model.part("key2_1").is_some());
        assert!(s.model.part("key2_2").is_none(), "row 2 has 2 keys");
        let p = at(&s, "key2_1");
        assert!((p - Vec3::new(0.02, 0.105, 2.0)).length() < 1e-5, "{p:?}");
        // Each copy keeps its own template: a param change re-derives it.
        s.apply_values_as(
            Some([("h".to_string(), 0.2)].into_iter().collect()),
            vec![],
            true,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert!((at(&s, "key2_1").y - 0.205).abs() < 1e-5);
        // Two copies making the same part: an error naming the variable.
        let dup = s.apply_values_as(
            None,
            vec![
                serde_json::json!({"type": "repeat", "var": "r", "count": 2, "items": [
                    {"type": "primitive", "name": "same", "kind": "box", "size": 0.01}
                ]}),
            ],
            true,
        );
        assert!(dup.is_err_and(|e| e.to_string().contains("{r}")));
        // A bad inner item says which repeat value it came from.
        let bad = s.apply_values_as(
            None,
            vec![serde_json::json!({"type": "repeat", "var": "r", "values": ["box", "blob"], "items": [
                {"type": "primitive", "name": "b{r}", "kind": "{r}", "size": 0.1}
            ]})],
            true,
        );
        let Err(e) = bad else {
            panic!("bad size should fail");
        };
        assert!(e.to_string().contains("repeat r=blob"), "{e}");
    }

    #[test]
    fn a_failed_batch_leaves_the_history_as_it_was() {
        let mut s = Session::default();
        let ok: Vec<serde_json::Value> = serde_json::from_str(
            r#"[{"type":"primitive","name":"a","kind":"box","size":1,"translation":[0,0.5,0]}]"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s.apply_values(None, ok).unwrap_or_else(|e| panic!("{e}"));
        let (nodes, cur) = (s.model.history.nodes.len(), s.model.history.current);
        let kids = cur.map(|c| s.model.history.nodes[c].children.len());
        // The second item reads a part that doesn't exist: the call fails
        // after the first item was applied and committed inside the batch.
        let bad: Vec<serde_json::Value> = serde_json::from_str(
            r#"[{"type":"primitive","name":"b","kind":"box","size":1},
                {"type":"transform","name":"c","input":"nope"}]"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(s.apply_values(None, bad).is_err());
        assert_eq!(s.model.history.nodes.len(), nodes);
        assert_eq!(s.model.history.current, cur);
        assert_eq!(cur.map(|c| s.model.history.nodes[c].children.len()), kids);
        assert!(s.model.parts.iter().all(|p| p.name != "b"));
    }

    #[test]
    fn a_flat_colour_warns_unless_marked_flat() {
        let mut s = Session::default();
        let items = |flat: bool| -> Vec<serde_json::Value> {
            serde_json::from_str(&format!(
                r##"[{{"type":"primitive","name":"b","kind":"box","size":1,"translation":[0,0.5,0]}},
                    {{"type":"material","name":"b","color":"#c33","flat":{flat}}}]"##
            ))
            .unwrap_or_else(|e| panic!("{e}"))
        };
        s.apply_values(None, items(false))
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(s.lints().iter().any(|l| l.contains("only a flat colour")));
        s.apply_values(None, items(true))
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(!s.lints().iter().any(|l| l.contains("only a flat colour")));
        assert!(material_gaps(&s.model).1.is_empty());
    }

    #[test]
    fn a_material_item_takes_a_name_pattern() {
        let mut s = Session::default();
        run(
            &mut s,
            serde_json::json!([
                {"type": "primitive", "name": "wall_a", "kind": "box", "size": 0.1, "translation": [0, 0.05, 0]},
                {"type": "primitive", "name": "wall_b", "kind": "box", "size": 0.1, "translation": [0.2, 0.05, 0]},
                {"type": "primitive", "name": "floor", "kind": "box", "size": 0.1, "translation": [0.4, 0.05, 0]},
                {"type": "material", "name": "wall_*", "color": "#c33"}
            ]),
        );
        let col = |n: &str| {
            s.model
                .parts
                .iter()
                .find(|p| p.name == n)
                .map(|p| p.material.base_color)
        };
        assert_ne!(col("wall_a"), col("floor"));
        assert_eq!(col("wall_a"), col("wall_b"));
        let err = s
            .apply_values(
                None,
                vec![serde_json::json!({"type": "material", "name": "nope_*", "color": "#c33"})],
            )
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(err.contains("matches no part"), "{err}");
    }

    #[test]
    fn editing_a_material_reapplies_it_to_parts() {
        let mut s = Session::default();
        let mat =
            |c: &str| vec![serde_json::json!({"type": "output", "name": "out", "base_color": c})];
        s.materials
            .ops("m", mat("#ff0000"), None, None, None, Some(16), None)
            .unwrap_or_else(|e| panic!("{e}"));
        run(
            &mut s,
            serde_json::json!([
                {"type": "primitive", "name": "b", "kind": "box", "size": 0.1, "translation": [0, 0.05, 0]},
                {"type": "material", "name": "b", "pbr": "m"}
            ]),
        );
        let key = |s: &Session| {
            s.model.ops.iter().find_map(|o| match o {
                Op::Material { pbr: Some(r), .. } => r.graph.as_ref().map(|g| format!("{g:?}")),
                _ => None,
            })
        };
        let before = key(&s);
        s.materials
            .ops("m", mat("#0000ff"), None, None, None, Some(16), None)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.refresh_pbr_users("m"), Ok(1));
        assert_ne!(before, key(&s));
    }

    #[test]
    fn a_failed_coverage_says_where_the_values_sit() {
        let mut s = Session::default();
        let ops = vec![
            serde_json::json!({"type": "noise", "name": "n", "size": 0.1}),
            serde_json::json!({"type": "levels", "name": "m", "input": "n", "in": [0.95, 0.99]}),
            serde_json::json!({"type": "output", "name": "out", "base_color": "m"}),
        ];
        let expect: crate::material::MatExpect = serde_json::from_value(
            serde_json::json!({"coverage": [{"node": "m", "min": 0.3, "max": 0.5}]}),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let (text, ok) = s
            .materials
            .ops("cov", ops, None, None, None, Some(32), Some(&expect))
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(!ok);
        assert!(text.contains("input `n` is above"), "{text}");
    }

    #[test]
    fn verify_materials_flags_grey_and_flat_parts() {
        let mut s = Session::default();
        run(
            &mut s,
            serde_json::json!([
                {"type": "primitive", "name": "a", "kind": "box", "size": 0.1, "translation": [0, 0.05, 0]},
                {"type": "primitive", "name": "b", "kind": "box", "size": 0.1, "translation": [0.2, 0.05, 0]},
                {"type": "material", "name": "b", "color": "#c33"}
            ]),
        );
        let check = |s: &Session, v: serde_json::Value| {
            let spec: VerifySpec = serde_json::from_value(serde_json::json!({"materials": v}))
                .unwrap_or_else(|e| panic!("{e}"));
            s.verify_checks(&spec)
                .into_iter()
                .find(|(n, _, _)| n == "materials")
                .unwrap_or_else(|| panic!("no materials check"))
        };
        let (_, ok, text) = check(&s, serde_json::json!(true));
        assert!(
            !ok && text.contains("`a`") && !text.contains("`b`"),
            "{text}"
        );
        run(
            &mut s,
            serde_json::json!([{"type": "material", "name": "a", "color": "#333"}]),
        );
        assert!(check(&s, serde_json::json!(true)).1);
        let (_, ok, text) = check(&s, serde_json::json!("pbr"));
        assert!(!ok && text.contains("flat colour"), "{text}");
    }

    #[test]
    fn reissuing_an_in_place_relative_transform_replaces_it() {
        let mut s = Session::default();
        let t = |deg: f32| {
            serde_json::json!({"type": "transform", "name": "w", "input": "w", "relative": true,
                               "scale": [1, 1, 2], "rotation": {"x": deg}})
        };
        run(
            &mut s,
            serde_json::json!([{"type": "primitive", "name": "w", "kind": "box", "size": 0.1}, t(-10.0)]),
        );
        run(&mut s, serde_json::json!([t(-6.0)]));
        let n = s
            .model
            .ops
            .iter()
            .filter(|o| graph::kind(o) == "transform")
            .count();
        assert_eq!(n, 1, "the re-issue replaced the first transform");
        let Some(p) = s.model.part("w") else {
            panic!("no w");
        };
        assert!(
            (p.scale[2] - 2.0).abs() < 1e-5,
            "scaled once: {:?}",
            p.scale
        );
        let want = bevy::math::Quat::from_rotation_x((-6.0f32).to_radians());
        assert!(crate::quat_of(p.rotation).dot(want).abs() > 1.0 - 1e-5);
    }

    #[test]
    fn place_each_drops_every_part_onto_a_slope() {
        let mut s = Session::default();
        run(
            &mut s,
            serde_json::json!([
                {"type": "primitive", "name": "deck", "kind": "box", "size": [1, 0.05, 0.4], "translation": [0, 0.3, 0], "rotation": {"z": 15}},
                {"type": "primitive", "name": "key", "kind": "box", "size": [0.04, 0.02, 0.04], "translation": [-0.4, 1, 0]},
                {"type": "linear_array", "name": "key", "input": "key", "count": 10, "offset": [0.09, 0, 0]},
                {"type": "place", "name": "keys_on", "parts": ["key*"], "on": "deck", "each": true}
            ]),
        );
        let mut last = f32::MIN;
        for i in 0..10 {
            let n = if i == 0 {
                "key".to_string()
            } else {
                format!("key_{i}")
            };
            let Some(p) = s.model.part(&n) else {
                panic!("no {n}");
            };
            let (lo, hi) = p.bounds();
            assert!(
                (lo.x + hi.x) * 0.5 - (-0.4 + 0.09 * i as f32) < 1e-4,
                "{n} slid"
            );
            assert!(lo.y > last, "{n} at {} not above the previous key", lo.y);
            last = lo.y;
            crate::contact::rests_on(&s.model, &n, "deck").unwrap_or_else(|e| panic!("{n}: {e}"));
        }
        let bad = s.apply_values_as(
            None,
            vec![serde_json::json!({"type": "place", "parts": ["key*"], "on": "deck", "each": true, "at": [0, 0]})],
            true,
        );
        assert!(bad.is_err());
    }

    #[test]
    fn anchor_pivots_resolve_and_reissue_does_not_drift() {
        let mut s = Session::default();
        run(
            &mut s,
            serde_json::json!([
                {"type": "primitive", "name": "base", "kind": "box", "size": [0.3, 0.02, 0.2], "translation": [0, 0.01, 0]},
                {"type": "primitive", "name": "lid", "kind": "box", "size": [0.3, 0.2, 0.01], "translation": [0, 0.12, -0.095]},
                {"type": "move", "name": "hinge", "parts": ["lid"], "axis": [1, 0, 0], "degrees": -20, "pivot": "lid:bottom-back"}
            ]),
        );
        let first = s
            .model
            .part("lid")
            .map(|p| p.translation)
            .unwrap_or_default();
        run(
            &mut s,
            serde_json::json!([
                {"type": "move", "name": "hinge", "parts": ["lid"], "axis": [1, 0, 0], "degrees": -20, "pivot": "lid:bottom-back"}
            ]),
        );
        let again = s
            .model
            .part("lid")
            .map(|p| p.translation)
            .unwrap_or_default();
        for k in 0..3 {
            assert!((first[k] - again[k]).abs() < 1e-5, "{first:?} {again:?}");
        }
        // The hinge edge stays put: the lid's lowest back point is still at y 0.02.
        let (lo, _) = s.model.part("lid").map(|p| p.bounds()).unwrap_or_default();
        assert!((lo.y - 0.02).abs() < 1e-4, "{lo:?}");
        assert!(anchor_point("sideways", Vec3::ZERO, Vec3::ONE).is_err());
    }

    #[test]
    fn a_failing_batch_rolls_back_entirely() {
        let mut s = Session::default();
        let before = s.model.ops.len();
        let r = s.apply_values_as(
            None,
            vec![
                serde_json::json!({"type": "primitive", "name": "a", "kind": "box", "size": 1}),
                serde_json::json!({"type": "boolean", "name": "c", "op": "union", "a": "a", "b": "ghost"}),
            ],
            true,
        );
        assert!(r.is_err_and(|e| e.to_string().contains("rolled back")));
        assert_eq!(s.model.ops.len(), before);
        assert!(s.model.part("a").is_none());
    }

    #[test]
    fn batch_items_may_read_parts_produced_later_in_the_batch() {
        let mut s = Session::default();
        s.apply_values_as(
            None,
            vec![
                serde_json::json!({"type": "hull", "name": "pill", "inputs": ["a", "b"]}),
                serde_json::json!({"type": "primitive", "name": "a", "kind": "sphere", "size": 0.1}),
                serde_json::json!({"type": "primitive", "name": "b", "kind": "sphere", "size": 0.1, "translation": [0, 0.5, 0]}),
                serde_json::json!({"type": "material", "name": "pill", "color": "#f00"}),
            ],
            true,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(s.model.part("pill").is_some());
    }

    #[test]
    fn pattern_hides_and_moves_see_parts_made_later_in_the_batch() {
        let mut s = Session::default();
        s.apply_values_as(
            None,
            vec![
                serde_json::json!({"type": "hide_part", "part": "*_raw"}),
                serde_json::json!({"type": "move", "name": "up", "parts": ["b*"], "translation": [0, 1, 0]}),
                serde_json::json!({"type": "primitive", "name": "top_raw", "kind": "box", "size": 1}),
                serde_json::json!({"type": "primitive", "name": "b", "kind": "box", "size": 1, "translation": [3, 0.5, 0]}),
            ],
            true,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(s.model.part("top_raw").is_some_and(|p| !p.visible));
        // A later explicit show still wins over an earlier pattern hide.
        s.apply_values_as(
            None,
            vec![
                serde_json::json!({"type": "hide_part", "part": "*_raw"}),
                serde_json::json!({"type": "show_part", "part": "top_raw"}),
            ],
            true,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(s.model.part("top_raw").is_some_and(|p| p.visible));
        let lo = s.model.part("b").map(|p| p.bounds().0).unwrap_or_default();
        assert!((lo.y - 1.0).abs() < 1e-4, "b at {lo:?}");
    }

    fn batch(items: serde_json::Value) -> (Session, String) {
        let mut s = Session::default();
        let serde_json::Value::Array(items) = items else {
            panic!("array");
        };
        let t = s
            .apply_values_as(None, items, true)
            .unwrap_or_else(|e| panic!("{e}"));
        (s, t)
    }

    #[test]
    fn a_baked_decimate_wears_its_detail_through_copies_and_drops_it_on_reshaping() {
        let mut s = Session::default();
        let arr = |v: serde_json::Value| v.as_array().cloned().unwrap_or_default();
        let items = serde_json::json!([
            {"type": "primitive", "name": "ball", "kind": "sphere", "size": 0.3, "translation": [0, 0.3, 0]},
            {"type": "deform", "name": "rock_hi", "input": "ball", "mode": "displace", "amount": 0.02, "scale": 0.08, "detail": 0.01},
            {"type": "decimate", "name": "rock", "input": "rock_hi", "tris": 300, "bake": true, "resolution": 128},
            {"type": "mirror", "name": "rock_m", "input": "rock", "axis": "x", "offset": [0.5, 0, 0]},
            {"type": "deform", "name": "rock_bent", "input": "rock", "mode": "twist", "degrees": 20},
            {"type": "material", "name": "rock*", "color": "#887766"},
            {"type": "hide_part", "part": "ball"},
            {"type": "hide_part", "part": "rock_hi"}
        ]);
        let out = s
            .apply_values_as(None, arr(items), false)
            .unwrap_or_else(|e| panic!("{e}"));
        let part = |n: &str| s.model.part(n).unwrap_or_else(|| panic!("{n}"));
        let rock = part("rock");
        assert!(rock.tri_count() <= 330, "{}", rock.tri_count());
        // A flat colour still warns (the carrier graph is not a choice).
        assert!(out.contains("rock"), "{out}");
        let r = rock
            .material
            .pbr
            .as_ref()
            .unwrap_or_else(|| panic!("carrier"));
        assert_eq!(r.material, crate::decimate::PLAIN);
        assert!(!rock.material.has_pbr());
        let ch = crate::material::pbr::bake_part(rock)
            .unwrap_or_else(|e| panic!("{e}"))
            .unwrap_or_else(|| panic!("bake"));
        assert_eq!(ch.w, 128);
        assert!(ch.normal.iter().filter(|n| n[2] < 0.98).count() > 128 * 128 / 20);
        assert!(part("rock_m").detail.is_some(), "mirror keeps the bake");
        assert!(part("rock_bent").detail.is_none(), "a twist reshapes it");
        let (_, flat) = material_gaps(&s.model);
        assert!(flat.contains(&"rock".to_string()), "{flat:?}");
    }

    #[test]
    fn pbr_materials_map_parts_at_real_scale_and_follow_lineage() {
        let mut s = Session::default();
        let arr = |v: serde_json::Value| v.as_array().cloned().unwrap_or_default();
        s.materials
            .ops(
                "tiles",
                arr(serde_json::json!([
                    {"type": "tile_grid", "name": "g", "size": [0.1, 0.1]},
                    {"type": "math", "name": "m", "op": "step", "a": "g.edge", "b": "$gap"},
                    {"type": "output", "name": "out", "base_color": "m"}
                ])),
                Some(crate::params::Params::from([("gap".to_string(), 0.004)])),
                None,
                Some(0.5),
                Some(64),
                None,
            )
            .unwrap_or_else(|e| panic!("{e}"));
        s.materials
            .ops(
                "marble",
                arr(serde_json::json!([
                    {"type": "input", "name": "p", "of": "position"},
                    {"type": "noise", "name": "n", "coords": "p", "size": 0.1},
                    {"type": "output", "name": "out", "base_color": "n"}
                ])),
                None,
                Some("object".into()),
                None,
                Some(64),
                None,
            )
            .unwrap_or_else(|e| panic!("{e}"));
        let items = serde_json::json!([
            {"type": "primitive", "name": "wall", "kind": "box", "size": [2.0, 1.0, 0.1], "translation": [0, 0.5, 0]},
            {"type": "material", "name": "wall", "pbr": "tiles", "params": {"gap": 0.01}},
            {"type": "linear_array", "name": "walls", "input": "wall", "count": 2, "offset": [0, 0, 1]},
            {"type": "primitive", "name": "top", "kind": "box", "size": [0.5, 0.05, 0.5], "translation": [0, 1.2, 0]},
            {"type": "material", "name": "top", "pbr": "marble"}
        ]);
        s.apply_values_as(None, arr(items), false)
            .unwrap_or_else(|e| panic!("{e}"));
        let wall = s.model.part("walls_1").unwrap_or_else(|| panic!("walls_1"));
        let r = wall
            .material
            .pbr
            .as_ref()
            .unwrap_or_else(|| panic!("pbr inherited"));
        assert_eq!(r.params.get("gap"), Some(&0.01));
        // 2 m wall / 0.5 m tile = 4 repeats across.
        let span = wall.uvs.iter().fold((f32::MAX, f32::MIN), |(a, b), uv| {
            (a.min(uv[0]), b.max(uv[0]))
        });
        assert!((span.1 - span.0 - 4.0).abs() < 1e-4, "{span:?}");
        let ch = crate::material::pbr::bake_part(wall)
            .unwrap_or_else(|e| panic!("{e}"))
            .unwrap_or_else(|| panic!("bake"));
        assert_eq!(ch.w, 64);
        // Object material: atlas uvs inside 0..1 and a per-part bake.
        let top = s.model.part("top").unwrap_or_else(|| panic!("top"));
        assert!(
            top.uvs
                .iter()
                .all(|uv| (0.0..=1.0).contains(&uv[0]) && (0.0..=1.0).contains(&uv[1]))
        );
        assert!(
            crate::material::pbr::bake_part(top)
                .unwrap_or_else(|e| panic!("{e}"))
                .is_some()
        );
        // An unknown material or param names what exists.
        let e = s
            .apply_values_as(
                None,
                arr(serde_json::json!([{"type": "material", "name": "top", "pbr": "nope"}])),
                false,
            )
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(e.contains("working: marble, tiles"), "{e}");
        let e = s
            .apply_values_as(
                None,
                arr(serde_json::json!([{"type": "material", "name": "top", "pbr": "tiles", "params": {"gapp": 1}}])),
                false,
            )
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(e.contains("no param `gapp` (params: gap)"), "{e}");
        // Prefab files carry the graph: a round trip keeps the material.
        let text = serde_json::to_string(&crate::Doc::for_file(&s.model)).unwrap_or_default();
        assert!(text.contains("tile_grid"));
    }

    #[test]
    fn collisions_fail_deep_overlaps_and_list_shallow_joints() {
        let (s, _) = batch(serde_json::json!([
            {"type": "primitive", "name": "cab", "kind": "box", "size": [0.6, 0.8, 0.6], "translation": [0, 0.4, 0]},
            // Half buried in the cabinet: a collision.
            {"type": "primitive", "name": "door", "kind": "box", "size": [0.5, 0.6, 0.04], "translation": [0, 0.4, 0.3]},
            // A tap sunk 1 cm into the top: a joint.
            {"type": "primitive", "name": "tap", "kind": "box", "size": [0.02, 0.3, 0.02], "translation": [0.2, 0.94, 0]}
        ]));
        let spec = |j: serde_json::Value| -> VerifySpec {
            serde_json::from_value(j).unwrap_or_else(|e| panic!("{e}"))
        };
        let checks = s.verify_checks(&spec(serde_json::json!({"collisions": true})));
        let (_, ok, detail) = &checks[0];
        assert!(!ok, "{detail}");
        assert!(
            detail.contains("`cab` / `door`") && detail.contains("50%"),
            "{detail}"
        );
        assert!(
            detail.contains("joint") && detail.contains("tap"),
            "{detail}"
        );
        let checks = s.verify_checks(&spec(
            serde_json::json!({"collisions": {"allow": [["door", "cab*"]]}}),
        ));
        assert!(checks[0].1, "{}", checks[0].2);
        // A blank left visible next to what was made from it says so.
        let (s, _) = batch(serde_json::json!([
            {"type": "primitive", "name": "raw", "kind": "box", "size": 1, "translation": [0, 0.5, 0]},
            {"type": "shell", "name": "tub", "input": "raw", "thickness": 0.05, "open": "top"}
        ]));
        let checks = s.verify_checks(&spec(serde_json::json!({"collisions": true})));
        assert!(
            checks[0]
                .2
                .contains("`raw` is an input of `tub` and still visible"),
            "{}",
            checks[0].2
        );
    }

    #[test]
    fn move_reports_direction_left_behind_parts_and_new_overlaps() {
        let (mut s, _) = batch(serde_json::json!([
            {"type": "primitive", "name": "cab", "kind": "box", "size": [0.6, 0.8, 0.6], "translation": [0, 0.4, 0]},
            {"type": "primitive", "name": "door", "kind": "box", "size": [0.56, 0.7, 0.02], "translation": [0, 0.4, 0.31]},
            {"type": "primitive", "name": "handle", "kind": "box", "size": [0.02, 0.1, 0.02], "translation": [0.2, 0.5, 0.33]}
        ]));
        // Hinged on the wrong side and without its handle: swings into the box.
        let t = s
            .apply_values_as(
                None,
                vec![serde_json::json!({"type": "move", "name": "open", "parts": ["door"], "axis": [0, 1, 0], "degrees": -70, "pivot": "door:right-front"})],
                true,
            )
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(t.contains("back (-z)"), "{t}");
        assert!(t.contains("left behind: `handle`"), "{t}");
        assert!(t.contains("new overlap") && t.contains("`cab`"), "{t}");
        // The right way: outward, handle along. No warnings.
        let t = s
            .apply_values_as(
                None,
                vec![serde_json::json!({"type": "move", "name": "open", "parts": ["door", "handle"], "axis": [0, 1, 0], "degrees": 70, "pivot": "door:right-back"})],
                true,
            )
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(t.contains("front (+z)") && !t.contains("WARNING"), "{t}");
    }

    #[test]
    fn z_fight_warning_names_the_visible_boolean_input() {
        let (s, _) = batch(serde_json::json!([
            {"type": "primitive", "name": "top_raw", "kind": "box", "size": [1, 0.04, 0.6], "translation": [0, 0.9, 0]},
            {"type": "primitive", "name": "hole", "kind": "box", "size": [0.4, 0.2, 0.3], "translation": [0, 0.9, 0]},
            {"type": "boolean", "name": "top", "op": "difference", "a": "top_raw", "b": "hole"},
            {"type": "hide_part", "part": "hole"}
        ]));
        let l = s.lints().join("\n");
        assert!(
            l.contains("`top_raw` is an input of the boolean that makes `top`"),
            "{l}"
        );
    }

    #[test]
    fn shell_hollows_a_box_with_exact_walls_and_an_open_face() {
        let (s, _) = batch(serde_json::json!([
            {"type": "primitive", "name": "raw", "kind": "box", "size": [0.6, 0.8, 0.5], "translation": [0, 0.4, 0]},
            {"type": "shell", "name": "carcass", "input": "raw", "thickness": 0.02, "open": "front"},
            {"type": "hide_part", "part": "raw"}
        ]));
        let Some(p) = s.model.part("carcass") else {
            panic!("no carcass");
        };
        let (v, _) = crate::inspect::volume_centroid(p);
        // Outer minus a cavity 0.56 x 0.76 that runs out through the front.
        let want = 0.6 * 0.8 * 0.5 - 0.56 * 0.76 * 0.48;
        assert!((v - want).abs() < 1e-5, "volume {v} vs {want}");
        // A ray from the front enters the opening and stops at the back wall.
        let t = crate::contact::probe_text(&s.model, [0.0, 0.4, 2.0], [0.0, 0.0, -1.0], false)
            .unwrap_or_else(|e| e);
        assert!(t.contains("point [0.0000, 0.4000, -0.2300]"), "{t}");
        // Thicker than half the box: a clear error.
        let mut s2 = Session::default();
        let e = s2
            .apply_values_as(
                None,
                vec![
                    serde_json::json!({"type": "primitive", "name": "raw", "kind": "box", "size": 0.1}),
                    serde_json::json!({"type": "shell", "name": "c", "input": "raw", "thickness": 0.06}),
                ],
                true,
            )
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(e.contains("leaves no inside"), "{e}");
    }

    #[test]
    fn place_toward_a_wall_mounts_parts_flush() {
        let (s, t) = batch(serde_json::json!([
            {"type": "primitive", "name": "wall_left", "kind": "box", "size": [0.1, 2.6, 4], "translation": [-2.55, 1.3, 0]},
            {"type": "primitive", "name": "wall_back", "kind": "box", "size": [5, 2.6, 0.1], "translation": [0, 1.3, -2.05]},
            {"type": "primitive", "name": "frame", "kind": "box", "size": [0.04, 0.5, 0.7], "translation": [0, 0, 0]},
            {"type": "primitive", "name": "bracket", "kind": "box", "size": [0.03, 0.15, 0.2], "translation": [0, 0, 0]},
            {"type": "place", "name": "hang", "parts": ["frame"], "on": "wall_left", "toward": "left", "at": [-0.4, 1.5]},
            {"type": "place", "name": "fix", "parts": ["bracket"], "on": "wall_back", "toward": "back", "at": [-1.6, 1.6]}
        ]));
        let b = |n: &str| s.model.part(n).map(|p| p.bounds()).unwrap_or_default();
        let (lo, hi) = b("frame");
        assert!((lo.x + 2.5).abs() < 1e-4, "frame {lo:?}");
        assert!(
            ((lo.z + hi.z) * 0.5 + 0.4).abs() < 1e-4 && ((lo.y + hi.y) * 0.5 - 1.5).abs() < 1e-4
        );
        let (lo, hi) = b("bracket");
        assert!((lo.z + 2.0).abs() < 1e-4, "bracket {lo:?}");
        assert!(((lo.x + hi.x) * 0.5 + 1.6).abs() < 1e-4, "{lo:?} {hi:?}");
        assert!(t.contains("moved left against `wall_left`"), "{t}");
        // A place onto a part that another place moves runs after it, in
        // whatever order they were written.
        let mut s = s;
        s.apply_values_as(
            None,
            vec![
                serde_json::json!({"type": "primitive", "name": "pot", "kind": "box", "size": 0.1}),
                serde_json::json!({"type": "place", "name": "pot_on", "parts": ["pot"], "on": "shelf", "at": [-1.2, -1.9]}),
                serde_json::json!({"type": "primitive", "name": "shelf", "kind": "box", "size": [1, 0.03, 0.25]}),
                serde_json::json!({"type": "place", "name": "shelf_on", "parts": ["shelf"], "on": "wall_back", "toward": "back", "at": [-1.2, 1.7]}),
            ],
            true,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let (lo, _) = s.model.part("pot").map(|p| p.bounds()).unwrap_or_default();
        assert!((lo.y - 1.715).abs() < 1e-4, "pot {lo:?}");
        assert!(crate::contact::floating(&s.model).is_empty());
    }

    #[test]
    fn move_turns_a_set_of_parts_as_one_body() {
        let mut s = Session::default();
        s.new_model("t").unwrap_or_else(|e| panic!("{e}"));
        let v = |j: serde_json::Value| -> Vec<serde_json::Value> {
            serde_json::from_value(j).unwrap_or_else(|e| panic!("{e}"))
        };
        s.apply_values(
            None,
            v(serde_json::json!([
                {"type": "primitive", "name": "a", "kind": "box", "size": 1, "translation": [0, 0.5, 0]},
                {"type": "linear_array", "name": "a", "input": "a", "count": 2, "offset": [2, 0, 0]}
            ])),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        // Shift everything up 1: the copy must move once, not twice.
        s.apply_values(
            None,
            v(serde_json::json!([{"type": "move", "parts": ["*"], "translation": [0, 1, 0]}])),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        for n in ["a", "a_1"] {
            let (lo, _) = s.model.part(n).map(|p| p.bounds()).unwrap_or_default();
            assert!((lo.y - 1.0).abs() < 1e-4, "{n} at {lo:?}");
        }
        // Quarter turn about z through [1, 0, 0]: the pair stands up.
        s.apply_values(
            None,
            v(serde_json::json!([{"type": "move", "parts": ["a*"], "axis": [0, 0, 1], "degrees": 90, "pivot": [-0.5, 1, 0]}])),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let (lo, hi) = s.model.bounds().unwrap_or_default();
        assert!((hi.x - lo.x - 1.0).abs() < 1e-3, "{lo:?}..{hi:?}");
        assert!((hi.y - lo.y - 3.0).abs() < 1e-3, "{lo:?}..{hi:?}");
        assert!((lo.y - 1.0).abs() < 1e-3, "{lo:?}");
    }

    #[test]
    fn move_tilts_a_mirror_once() {
        let mut s = Session::default();
        s.new_model("t").unwrap_or_else(|e| panic!("{e}"));
        let v = |j: serde_json::Value| -> Vec<serde_json::Value> {
            serde_json::from_value(j).unwrap_or_else(|e| panic!("{e}"))
        };
        s.apply_values(
            None,
            v(serde_json::json!([
                {"type": "primitive", "name": "a", "kind": "box", "size": [0.1, 1, 0.1], "translation": [0, 0.5, 0.3]},
                {"type": "mirror", "name": "a_m", "input": "a", "axis": "z"},
                {"type": "move", "parts": ["*"], "axis": [1, 0, 0], "degrees": 10, "pivot": [0, 0, 0]}
            ])),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let (alo, ahi) = s.model.part("a").map(|p| p.bounds()).unwrap_or_default();
        let (mlo, mhi) = s.model.part("a_m").map(|p| p.bounds()).unwrap_or_default();
        // Rigid: both lean by the same 10° (a double tilt would widen the
        // mirror's z extent), and the mirror's foot lifts as it swings.
        assert!(
            ((ahi.z - alo.z) - (mhi.z - mlo.z)).abs() < 1e-4,
            "a {alo:?}..{ahi:?} m {mlo:?}..{mhi:?}"
        );
        assert!(
            mlo.y > 0.04 && alo.y < -0.06,
            "a {alo:?}..{ahi:?} m {mlo:?}..{mhi:?}"
        );
    }

    #[test]
    fn move_in_a_later_batch_tilts_a_mirror_once() {
        let mut s = Session::default();
        s.new_model("t").unwrap_or_else(|e| panic!("{e}"));
        let v = |j: serde_json::Value| -> Vec<serde_json::Value> {
            serde_json::from_value(j).unwrap_or_else(|e| panic!("{e}"))
        };
        s.apply_values(
            None,
            v(serde_json::json!([
                {"type": "sweep", "name": "a", "profile": [[0.01, 0.01], [-0.01, 0.01], [-0.01, -0.01], [0.01, -0.01]], "path": [[0, 0, 0.0], [0, 1, 0.3]]},
                {"type": "mirror", "name": "a_m", "input": "a", "axis": "z"},
                {"type": "material", "name": "a", "color": "#f00"}
            ])),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let before = s.model.part("a_m").map(|p| p.bounds()).unwrap_or_default();
        s.apply_values(
            None,
            v(serde_json::json!([{"type": "move", "parts": ["*"], "axis": [1, 0, 0], "degrees": 10, "pivot": [0, 0, 0]}])),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let (mlo, mhi) = s.model.part("a_m").map(|p| p.bounds()).unwrap_or_default();
        // a_m rotated rigidly: its top end (0, 1, -0.3) goes to
        // (0, cos10 + 0.3 sin10, sin10 - 0.3 cos10).
        let (c, sn) = (10f32.to_radians().cos(), 10f32.to_radians().sin());
        let top_z = sn - 0.3 * c;
        assert!(
            (mlo.z - top_z).abs() < 0.02,
            "before {before:?} after {mlo:?}..{mhi:?}, want min z ~{top_z}"
        );
    }

    #[test]
    fn reissuing_a_named_move_adjusts_it() {
        let mut s = Session::default();
        s.new_model("t").unwrap_or_else(|e| panic!("{e}"));
        let v = |j: serde_json::Value| -> Vec<serde_json::Value> {
            serde_json::from_value(j).unwrap_or_else(|e| panic!("{e}"))
        };
        s.apply_values(
            None,
            v(serde_json::json!([
                {"type": "primitive", "name": "a", "kind": "box", "size": 1, "translation": [0, 0.5, 0]},
                {"type": "move", "name": "lift", "parts": ["*"], "translation": [0, 1, 0]}
            ])),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s.apply_values(
            None,
            v(serde_json::json!([{"type": "move", "name": "lift", "parts": ["*"], "translation": [0, 2, 0]}])),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let (lo, _) = s.model.bounds().unwrap_or_default();
        assert!((lo.y - 2.0).abs() < 1e-4, "{lo:?}");
        assert_eq!(
            s.model
                .ops
                .iter()
                .filter(|o| matches!(o, Op::Move { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn comments_on_groups_and_material_nodes() {
        let mut s = Session::default();
        let items = serde_json::json!([
            {"type": "primitive", "name": "book", "kind": "box", "size": [0.03, 0.2, 0.15], "translation": [0, 0.1, 0]},
            {"type": "linear_array", "name": "book", "input": "book", "count": 3, "offset": [0.04, 0, 0]}
        ]);
        s.apply_values_as(None, items.as_array().cloned().unwrap_or_default(), false)
            .unwrap_or_else(|e| panic!("{e}"));
        s.materials
            .ops(
                "stone",
                vec![serde_json::json!({"type": "output", "name": "out", "base_color": "#808080"})],
                None,
                None,
                None,
                Some(32),
                None,
            )
            .unwrap_or_else(|e| panic!("{e}"));
        let g = crate::AnnotationTarget::Group {
            label: "book ×3".into(),
            parts: vec!["book".into(), "book_1".into(), "book_2".into()],
        };
        let r = s
            .annotate(&g, "spines too even")
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(r.contains("group `book ×3`"), "{r}");
        let a = &s.model.annotations[0];
        assert!((a.position[0] - 0.04).abs() < 1e-4, "{:?}", a.position);
        let m = crate::AnnotationTarget::Material {
            material: "stone".into(),
            node: Some("out".into()),
        };
        s.annotate(&m, "too grey").unwrap_or_else(|e| panic!("{e}"));
        let bad = crate::AnnotationTarget::Material {
            material: "stone".into(),
            node: Some("nope".into()),
        };
        assert!(s.annotate(&bad, "x").is_err());
        let list = s.list_annotations();
        assert!(
            list.contains("material `stone` node `out` 'too grey'"),
            "{list}"
        );
        assert!(s.model.annotations[1].is_on(&m) && !s.model.annotations[0].is_on(&m));
        assert!(s.model.annotations[0].is_on(&g));
    }
}
