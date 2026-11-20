//! Motion capture: BVH files retargeted onto modeler rigs.
//!
//! A `mocap` item (`{"type": "mocap", "name", "skeleton", "file"}`) is a
//! `clip` whose tracks come from a BVH file: [`lower`] turns it into an
//! ordinary [`ClipDef`] (dense rotate keys per mapped bone at `fps`, the
//! root's translation, foot-planting IK entries, footstep events), so every
//! tool that plays, layers, exports or verifies clips sees a plain clip.
//!
//! Retargeting works in world space, per mapped bone:
//! 1. The source's rest pose (BVH zero pose; T-pose, A-pose, whatever the
//!    file has) is turned to face +z (left = +x, from its thighs), and
//!    mirrored when asked.
//! 2. Per bone, `A` takes the source rest bone direction (joint -> the
//!    joint of the mapped child) to the target's (head -> mapped child head,
//!    else head -> tail).
//! 3. Per frame, the source joint's world rotation delta from rest `D`
//!    (BVH joint frames are world-aligned at rest, so it is the joint's
//!    world rotation, in the corrected frame) gives the target bone's world
//!    rotation `D * A^-1 * R` (`R` its rest frame): the bone points where the
//!    source bone points, twist included.
//! 4. That is turned into the bone's local offset from rest under its
//!    (already posed) parent: `r = rl^-1 * P^-1 * W`.
//!
//! The root's translation is scaled by target / source leg length (units
//! cancel; `scale` overrides). Feet are planted with IK: contact spans
//! (ankle within `contact_height` of its lowest and slower than
//! `contact_speed`) hold the ankle at its mean position, blended in and out
//! over three frames; where the leg can't reach, the hips are lowered.

pub mod bvh;
pub mod map;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use bevy::math::{Quat, Vec3};
use serde::{Deserialize, Serialize};

use crate::anim::{
    Channel, ClipDef, ClipEvent, IkDef, IkTarget, Interp, RootMotionDef, Skeleton, TrackDef,
};
pub use bvh::{Bvh, parse};

/// Contact planting of a pair of end bones (feet or hands).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Plant {
    pub plant: bool,
    #[serde(default = "contact_h")]
    pub contact_height: f32,
    #[serde(default = "contact_s")]
    pub contact_speed: f32,
}

fn contact_h() -> f32 {
    0.03
}

fn contact_s() -> f32 {
    0.15
}

impl Plant {
    pub fn on() -> Plant {
        Plant {
            plant: true,
            contact_height: contact_h(),
            contact_speed: contact_s(),
        }
    }

    pub fn off() -> Plant {
        Plant {
            plant: false,
            ..Plant::on()
        }
    }
}

/// The `mocap` part of a clip: where the motion comes from and how it is
/// retargeted.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MocapDef {
    /// A BVH file in the project (read when the rig builds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Inline BVH text (instead of `file`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bvh: Option<String>,
    /// FNV-1a of the file content when it was applied (hex).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hash: String,
    /// Overrides of the automatic mapping: our bone -> BVH joint ("" unmaps).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub map: BTreeMap<String, String>,
    /// Only these explicit entries (no automatic mapping).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub map_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<[f32; 2]>,
    #[serde(default = "fps30")]
    pub fps: f32,
    /// Crossfade seconds for a seamless loop (0: none).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub blend: f32,
    /// Root translation scale (default target / source leg length).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f32>,
    /// Root motion axes kept as travel (None: in place, travel removed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_motion: Option<String>,
    #[serde(default = "Plant::on")]
    pub feet: Plant,
    #[serde(default = "Plant::off")]
    pub hands: Plant,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mirror: bool,
    /// Playback rate (2 = twice as fast).
    #[serde(default = "one")]
    pub speed: f32,
    /// Extra turn about +y (degrees), applied to the whole motion.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub facing: f32,
    /// Turn so the travel goes toward +z.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub facing_auto: bool,
    #[serde(default)]
    pub interp: Interp,
}

fn fps30() -> f32 {
    30.0
}

fn one() -> f32 {
    1.0
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

impl Default for MocapDef {
    fn default() -> Self {
        MocapDef {
            file: None,
            bvh: None,
            hash: String::new(),
            map: BTreeMap::new(),
            map_only: false,
            range: None,
            fps: 30.0,
            blend: 0.0,
            scale: None,
            root_motion: None,
            feet: Plant::on(),
            hands: Plant::off(),
            mirror: false,
            speed: 1.0,
            facing: 0.0,
            facing_auto: false,
            interp: Interp::Linear,
        }
    }
}

pub use crate::silhouette::content_hash;

fn hex(h: u64) -> String {
    format!("{h:016x}")
}

/// File contents the server read for a call, by hash (so a rebuild never
/// needs the project again in the same process).
static TEXTS: Mutex<Vec<(String, Arc<str>)>> = Mutex::new(Vec::new());
static PARSED: Mutex<Vec<(String, Arc<Bvh>)>> = Mutex::new(Vec::new());

/// Remembers a file's text; returns its hash (hex).
pub fn remember(text: &str) -> String {
    let h = hex(content_hash(text.as_bytes()));
    if let Ok(mut c) = TEXTS.lock()
        && !c.iter().any(|e| e.0 == h)
    {
        c.push((h.clone(), Arc::from(text)));
        if c.len() > 8 {
            c.remove(0);
        }
    }
    h
}

fn parsed(hash: &str, text: &str) -> Result<Arc<Bvh>, String> {
    if let Ok(c) = PARSED.lock()
        && let Some(e) = c.iter().find(|e| e.0 == hash)
    {
        return Ok(e.1.clone());
    }
    let b = Arc::new(parse(text)?);
    if let Ok(mut c) = PARSED.lock() {
        c.push((hash.to_string(), b.clone()));
        if c.len() > 8 {
            c.remove(0);
        }
    }
    Ok(b)
}

/// `file` inside the current project (no `..` or symlink out of it).
pub fn project_file(file: &str) -> Result<std::path::PathBuf, String> {
    let root = crate::catalog::project_root()
        .ok_or_else(|| format!("mocap file `{file}`: no project to read it from"))?;
    let root = root
        .canonicalize()
        .map_err(|e| format!("mocap file `{file}`: project {}: {e}", root.display()))?;
    let p = root
        .join(file)
        .canonicalize()
        .map_err(|e| format!("mocap file `{file}`: {e}"))?;
    if !p.starts_with(&root) {
        return Err(format!("mocap file `{file}` is outside the project"));
    }
    Ok(p)
}

fn live_text(def: &MocapDef) -> Option<(String, Arc<str>)> {
    if let Some(t) = &def.bvh {
        return Some((hex(content_hash(t.as_bytes())), Arc::from(t.as_str())));
    }
    let file = def.file.as_deref()?;
    if let Some(t) = project_file(file)
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
    {
        let h = remember(&t);
        return Some((h, Arc::from(t)));
    }
    let c = TEXTS.lock().ok()?;
    c.iter().find(|e| e.0 == def.hash).cloned()
}

/// A key of the motion's current content (a changed file re-derives).
pub fn live_key(def: &MocapDef) -> String {
    if def.bvh.is_some() {
        return String::new(); // inline text is in the op already
    }
    def.file
        .as_deref()
        .and_then(|f| project_file(f).ok())
        .and_then(|p| std::fs::read(p).ok())
        .map(|b| hex(content_hash(&b)))
        .unwrap_or_else(|| def.hash.clone())
}

/// The motion, or None when the file can't be read here.
pub fn source(def: &MocapDef) -> Result<Option<Arc<Bvh>>, String> {
    match live_text(def) {
        Some((h, t)) => parsed(&h, &t).map(Some),
        None => Ok(None),
    }
}

/// The server's pass over a batch: every `mocap` item with a `file` gets
/// its content checked (parsed: errors with line numbers) and its hash.
pub fn resolve_files(
    items: &mut [serde_json::Value],
    read: &dyn Fn(&str) -> Result<Vec<u8>, String>,
) -> Result<(), String> {
    fn one(
        v: &mut serde_json::Value,
        read: &dyn Fn(&str) -> Result<Vec<u8>, String>,
    ) -> Result<(), String> {
        let Some(o) = v.as_object_mut() else {
            return Ok(());
        };
        let ty = o.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if ty == "repeat" {
            if let Some(serde_json::Value::Array(a)) = o.get_mut("items") {
                for x in a {
                    one(x, read)?;
                }
            }
            return Ok(());
        }
        if ty != "mocap" {
            return Ok(());
        }
        let Some(file) = o.get("file").and_then(|f| f.as_str()).map(String::from) else {
            return Ok(());
        };
        if file.contains('{') || file.contains('$') {
            return Ok(()); // a repeat template: read once expanded
        }
        let bytes = read(&file)?;
        let text = String::from_utf8(bytes)
            .map_err(|_| format!("mocap file `{file}`: not a text (BVH) file"))?;
        parse(&text).map_err(|e| format!("mocap file `{file}`: {e}"))?;
        let h = remember(&text);
        o.insert("hash".into(), serde_json::Value::String(h));
        Ok(())
    }
    for (i, it) in items.iter_mut().enumerate() {
        one(it, read).map_err(|e| format!("op #{i}: {e}"))?;
    }
    Ok(())
}

/// The names of a batch's `mocap` items (also inside repeats, when the
/// name is literal).
pub fn item_names(items: &[serde_json::Value]) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(v: &serde_json::Value, out: &mut Vec<String>) {
        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if ty == "repeat"
            && let Some(a) = v.get("items").and_then(|a| a.as_array())
        {
            for x in a {
                walk(x, out);
            }
        }
        if ty == "mocap"
            && let Some(n) = v.get("name").and_then(|n| n.as_str())
            && !n.contains('{')
        {
            out.push(n.to_string());
        }
    }
    for v in items {
        walk(v, &mut out);
    }
    out
}

/// What lowering found, for the report and the post-build checks.
#[derive(Clone, Debug, Default)]
pub struct Lowered {
    pub lines: Vec<String>,
    /// Ground velocity [vx, vz] (m/s) of the motion.
    pub speed: [f32; 2],
    /// Whether travel was removed (in place).
    pub in_place: bool,
    /// (bone index, [(first frame, last frame, anchor)]).
    pub spans: Vec<(usize, Vec<(usize, usize, Vec3)>)>,
    pub dt: f32,
    pub frames: usize,
}

fn fmt_s(t: f32) -> String {
    format!("{t:.2}")
}

/// Per output frame: local rotation per target bone (None: unmapped) and
/// the root's world position.
#[derive(Clone)]
struct Frame {
    rots: Vec<Option<Quat>>,
    root: Vec3,
}

fn angle(a: Quat, b: Quat) -> f32 {
    2.0 * a.dot(b).abs().min(1.0).acos().to_degrees()
}

/// Lowers a mocap clip to a plain one on `skel`.
pub fn lower(name: &str, def: &ClipDef, skel: &Skeleton) -> Result<(ClipDef, Lowered), String> {
    let ctx = format!("mocap `{name}`");
    let Some(m) = &def.mocap else {
        return Err(format!("{ctx}: no motion"));
    };
    let Some(src) = source(m).map_err(|e| format!("{ctx}: {e}"))? else {
        // The file isn't readable here (another project, a restart without
        // it): a rest clip, so the model still derives.
        let mut d = def.clone();
        d.mocap = None;
        d.duration = 1.0;
        return Ok((
            d,
            Lowered {
                lines: vec![format!(
                    "{ctx}: file `{}` can't be read here: the clip is the rest pose until it is re-applied in its project",
                    m.file.as_deref().unwrap_or("?")
                )],
                ..Lowered::default()
            },
        ));
    };
    lower_with(name, def, m, &src, skel)
}

fn rot_y(deg: f32) -> Quat {
    Quat::from_rotation_y(deg.to_radians())
}

pub(crate) fn lower_with(
    name: &str,
    def: &ClipDef,
    m: &MocapDef,
    src: &Bvh,
    skel: &Skeleton,
) -> Result<(ClipDef, Lowered), String> {
    let ctx = format!("mocap `{name}`");
    let nb = skel.bones.len();
    let has = |n: &str| skel.bone(n).is_some();
    // --- mapping: target bone -> source joint
    let mut pairs: Vec<(String, usize)> = if m.map_only {
        Vec::new()
    } else {
        map::auto(src, &has)
    };
    for (ours, theirs) in &m.map {
        if !has(ours) {
            return Err(format!(
                "{ctx}: map: no bone `{ours}` in `{}` (bones: {})",
                skel.name,
                skel.bones
                    .iter()
                    .map(|b| b.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        pairs.retain(|(t, _)| t != ours);
        if theirs.is_empty() {
            continue;
        }
        let Some(j) = src.joints.iter().position(|j| j.name == *theirs) else {
            return Err(format!(
                "{ctx}: map.{ours}: the file has no joint `{theirs}` (joints: {})",
                src.joints
                    .iter()
                    .map(|j| j.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        };
        pairs.retain(|(_, s)| *s != j);
        pairs.push((ours.clone(), j));
    }
    if m.mirror {
        for p in &mut pairs {
            if let Some(o) = crate::anim::mirror_name(&p.0)
                .or_else(|| p.0.strip_suffix("_r").map(|b| format!("{b}_l")))
                && has(&o)
            {
                p.0 = o;
            }
        }
    }
    let mut src_of: Vec<Option<usize>> = vec![None; nb];
    for (t, j) in &pairs {
        if let Some(i) = skel.bone(t) {
            src_of[i] = Some(*j);
        }
    }
    let Some(root_src) = (0..src.joints.len()).find(|&j| src.joints[j].parent.is_none()) else {
        return Err(format!("{ctx}: the file has no root joint"));
    };
    // The target bone carrying the translation: the one mapped to the
    // source root, else the target's root mapped to anything.
    let Some(root_t) = (0..nb)
        .find(|&i| src_of[i] == Some(root_src))
        .or_else(|| (0..nb).find(|&i| src_of[i].is_some() && skel.bones[i].parent.is_none()))
        .or_else(|| (0..nb).find(|&i| src_of[i].is_some()))
    else {
        return Err(format!(
            "{ctx}: no bone of `{}` matches a joint of the file (joints: {}); give `map`",
            skel.name,
            src.joints
                .iter()
                .map(|j| j.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    };
    // --- rest correction: face +z, left = +x (from the thighs or arms)
    let rest = src.rest_positions();
    let side_pair = |a: &str, b: &str| -> Option<(usize, usize)> {
        let ia = skel.bone(a)?;
        let ib = skel.bone(b)?;
        Some((src_of[ia]?, src_of[ib]?))
    };
    let mut y_fix = Quat::IDENTITY;
    if let Some((l, r)) = side_pair("thigh_l", "thigh_r")
        .or_else(|| side_pair("shoulder_l", "shoulder_r"))
        .or_else(|| side_pair("clavicle_l", "clavicle_r"))
    {
        let mut left = rest[l] - rest[r];
        if m.mirror {
            // The pair was swapped: the file's own left is our right.
            left = -left;
        }
        left.y = 0.0;
        if left.length() > 1e-6 {
            let fwd = left.cross(Vec3::Y);
            y_fix = rot_y(-fwd.x.atan2(fwd.z).to_degrees());
        }
    }
    let refl = |p: Vec3| {
        if m.mirror {
            Vec3::new(-p.x, p.y, p.z)
        } else {
            p
        }
    };
    let refl_q = |q: Quat| {
        if m.mirror {
            Quat::from_xyzw(q.x, -q.y, -q.z, q.w)
        } else {
            q
        }
    };
    let corr_rest = |p: Vec3| refl(y_fix * p);
    // --- alignment per mapped bone
    let head = |i: usize| skel.bones[i].head;
    let mut align: Vec<Quat> = vec![Quat::IDENTITY; nb];
    for i in 0..nb {
        let Some(j) = src_of[i] else { continue };
        let b = &skel.bones[i];
        let kids: Vec<usize> = (0..nb)
            .filter(|&c| skel.bones[c].parent == Some(i))
            .filter(|&c| src_of[c].is_some_and(|k| k != j && src.is_ancestor(j, k)))
            .collect();
        let pick = kids.iter().copied().min_by(|&a, &c| {
            let da = (head(a) - b.tail).length();
            let dc = (head(c) - b.tail).length();
            da.total_cmp(&dc)
        });
        let (dt, ds) = match pick.and_then(|c| src_of[c].map(|k| (c, k))) {
            Some((c, k)) => (head(c) - b.head, corr_rest(rest[k]) - corr_rest(rest[j])),
            None => {
                let first = src.children(j).first().map(|&k| src.joints[k].offset);
                let off = first.or(src.joints[j].end).unwrap_or(Vec3::ZERO);
                (
                    b.tail - b.head,
                    corr_rest(rest[j] + off) - corr_rest(rest[j]),
                )
            }
        };
        if dt.length() > 1e-6 && ds.length() > 1e-6 {
            align[i] = Quat::from_rotation_arc(ds.normalize(), dt.normalize());
        }
    }
    // --- proportions
    let pts = src.rest_points();
    let s_min = pts.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
    let s_max = pts.iter().map(|p| p.y).fold(f32::NEG_INFINITY, f32::max);
    let s_height = s_max - s_min;
    let s_hip = rest[root_src].y - s_min;
    let t_min = skel
        .bones
        .iter()
        .flat_map(|b| [b.head.y, b.tail.y])
        .fold(f32::INFINITY, f32::min);
    let t_hip = head(root_t).y - t_min;
    let units = [
        ("m", 1.0f32),
        ("cm", 0.01),
        ("inches", 0.0254),
        ("mm", 0.001),
    ];
    let (unit_name, unit) = units
        .iter()
        .copied()
        .min_by(|a, b| {
            let e = |u: f32| ((s_height * u).max(1e-9) / 1.7).ln().abs();
            e(a.1).total_cmp(&e(b.1))
        })
        .unwrap_or(("m", 1.0));
    // Leg length (thigh joint to the ground) sets the scale where both
    // have thighs, else the hip height.
    let legs: Vec<(f32, f32)> = ["thigh_l", "thigh_r"]
        .iter()
        .filter_map(|n| {
            let i = skel.bone(n)?;
            Some((head(i).y - t_min, rest[src_of[i]?].y - s_min))
        })
        .filter(|(t, s)| *t > 1e-6 && *s > 1e-6)
        .collect();
    let (scale, by) = match m.scale {
        Some(s) => (s, "given"),
        None if !legs.is_empty() => {
            let t: f32 = legs.iter().map(|l| l.0).sum();
            let s: f32 = legs.iter().map(|l| l.1).sum();
            (t / s, "target / source leg length")
        }
        None if s_hip > 1e-6 && t_hip > 1e-6 => (t_hip / s_hip, "target / source hip height"),
        None => (unit, "file units"),
    };
    // --- timing
    let dur_src = src.duration();
    let [t0, t1] = m.range.unwrap_or([0.0, dur_src]);
    let (t0, t1) = (t0.clamp(0.0, dur_src), t1.clamp(0.0, dur_src));
    if t1 <= t0 {
        return Err(format!(
            "{ctx}: range [{t0}, {t1}]: empty (the file is {} s long)",
            fmt_s(dur_src)
        ));
    }
    if m.speed.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater)
        || m.fps.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater)
    {
        return Err(format!("{ctx}: speed and fps must be positive"));
    }
    let dur = (t1 - t0) / m.speed;
    let n = ((dur * m.fps).round() as usize).max(1) + 1;
    let dt = dur / (n - 1) as f32;
    // The floor is the lowest the feet go (every joint without mapped feet).
    let feet_src: Vec<usize> = (0..nb)
        .filter(|&i| {
            let n = &skel.bones[i].name;
            n.starts_with("ankle") || n.starts_with("ball") || n.starts_with("toe")
        })
        .filter_map(|i| src_of[i])
        .collect();
    let on_foot: Vec<bool> = (0..src.joints.len())
        .map(|j| feet_src.is_empty() || feet_src.iter().any(|&f| src.is_ancestor(f, j)))
        .collect();
    // --- sample the source in the corrected frame (before `facing`)
    let mut src_world: Vec<Vec<(Vec3, Quat)>> = Vec::with_capacity(n);
    let mut floor = f32::INFINITY;
    for k in 0..n {
        let ts = t0 + k as f32 * dt * m.speed;
        let w = src.world(&src.locals_at(ts));
        let c: Vec<(Vec3, Quat)> = w
            .iter()
            .map(|(p, q)| {
                (
                    corr_rest(*p),
                    refl_q((y_fix * *q * y_fix.inverse()).normalize()),
                )
            })
            .collect();
        for (j, jt) in src.joints.iter().enumerate() {
            if !on_foot[j] {
                continue;
            }
            floor = floor.min(c[j].0.y);
            if let Some(e) = jt.end {
                let q = refl_q(y_fix * w[j].1 * y_fix.inverse());
                floor = floor.min((c[j].0 + q * refl(y_fix * e)).y);
            }
        }
        src_world.push(c);
    }
    // A file whose feet come near y = 0 has its floor there (a dip through
    // it is the take's own); otherwise the lowest the feet go.
    if floor.abs() < 0.05 * s_height {
        floor = 0.0;
    }
    // facing: an extra turn of the whole performance
    let travel0 = src_world[n - 1][root_src].0 - src_world[0][root_src].0;
    let mut yaw = m.facing;
    if m.facing_auto && Vec3::new(travel0.x, 0.0, travel0.z).length() > 1e-3 * s_height.max(1e-3) {
        yaw += -travel0.x.atan2(travel0.z).to_degrees();
    }
    let ym = rot_y(yaw);
    let p0 = ym * src_world[0][root_src].0;
    // --- target frames
    let mut frames: Vec<Frame> = Vec::with_capacity(n);
    for c in &src_world {
        let mut rots = vec![None; nb];
        let mut world_rot: Vec<Quat> = vec![Quat::IDENTITY; nb];
        for i in 0..nb {
            let b = &skel.bones[i];
            let (_, rl) = b.rest_local(&skel.bones);
            let parent = b.parent.map_or(Quat::IDENTITY, |p| world_rot[p]);
            match src_of[i] {
                Some(j) => {
                    let d = (ym * c[j].1).normalize();
                    let w = (d * align[i].inverse() * b.rest).normalize();
                    let r = ((parent * rl).inverse() * w).normalize();
                    rots[i] = Some(r);
                    world_rot[i] = w;
                }
                None => world_rot[i] = (parent * rl).normalize(),
            }
        }
        let p = ym * c[root_src].0;
        let rh = head(root_t);
        let root = Vec3::new(
            rh.x + scale * (p.x - p0.x),
            t_min + t_hip + scale * (p.y - floor - s_hip),
            rh.z + scale * (p.z - p0.z),
        );
        frames.push(Frame { rots, root });
    }
    // --- travel
    let travel = frames[n - 1].root - frames[0].root;
    let vel = Vec3::new(travel.x, 0.0, travel.z) / dur.max(1e-6);
    let axes = m.root_motion.clone();
    let in_place = axes.is_none();
    for (k, f) in frames.iter_mut().enumerate() {
        f.root -= vel * (k as f32 * dt);
    }
    // --- loop
    let mut lines = Vec::new();
    let seam = |fr: &[Frame]| -> (f32, f32) {
        let (a, b) = (&fr[0], &fr[fr.len() - 1]);
        let ang = a
            .rots
            .iter()
            .zip(&b.rots)
            .filter_map(|(x, y)| Some(angle((*x)?, (*y)?)))
            .fold(0.0f32, f32::max);
        (ang, (a.root - b.root).length())
    };
    let mut dur = dur;
    if def.looping {
        let (a0, d0) = seam(&frames);
        let nb_f = (m.blend * m.fps).round() as usize;
        if m.blend > 0.0 {
            if nb_f < 1 || 2 * nb_f + 2 > n {
                return Err(format!(
                    "{ctx}: blend {} s: needs at least one frame and under half the clip ({} s)",
                    m.blend,
                    fmt_s(dur)
                ));
            }
            let start = n - 1 - nb_f;
            let mut out: Vec<Frame> = Vec::with_capacity(n - nb_f);
            for (k, fk) in frames.iter().enumerate().skip(nb_f) {
                if k < start {
                    out.push(fk.clone());
                    continue;
                }
                let u = (k - start) as f32 / nb_f as f32;
                let w = u * u * (3.0 - 2.0 * u);
                let other = &frames[k - start];
                let rots = fk
                    .rots
                    .iter()
                    .zip(&other.rots)
                    .map(|(a, b)| match (a, b) {
                        (Some(a), Some(b)) => {
                            let b = if a.dot(*b) < 0.0 { -*b } else { *b };
                            Some(a.slerp(b, w).normalize())
                        }
                        _ => *a,
                    })
                    .collect();
                let root = fk.root.lerp(other.root, w);
                out.push(Frame { rots, root });
            }
            frames = out;
            dur = (frames.len() - 1) as f32 * dt;
            let (a1, d1) = seam(&frames);
            lines.push(format!(
                "loop seam: {a0:.1}° / {:.0} mm before the {} s blend, {a1:.1}° / {:.0} mm after",
                d0 * 1000.0,
                m.blend,
                d1 * 1000.0
            ));
        } else {
            lines.push(format!(
                "loop seam: {a0:.1}° / {:.0} mm between the first and last frame (loop: \"blend\" crossfades it)",
                d0 * 1000.0
            ));
        }
    }
    let nf = frames.len();
    let time = |k: usize| k as f32 * dt;
    // root motion: put the travel back
    if !in_place {
        for (k, f) in frames.iter_mut().enumerate() {
            f.root += vel * time(k);
        }
    }
    // --- the pose of a frame
    let pose_of = |f: &Frame| -> crate::anim::Pose {
        let mut pose = skel.rest();
        for i in 0..nb {
            if let Some(r) = f.rots[i] {
                // Limits as the clip will clamp them.
                pose[i].1 = match &skel.bones[i].limits {
                    Some(l) => crate::anim::ik::clamp(r, l),
                    None => r,
                };
            }
        }
        let b = &skel.bones[root_t];
        let (rlt, rl) = b.rest_local(&skel.bones);
        let (pp, pr) = match b.parent {
            Some(p) => {
                let w = skel.world(&pose);
                let (_, r, t) = w[p].to_scale_rotation_translation();
                (t, r)
            }
            None => (Vec3::ZERO, Quat::IDENTITY),
        };
        pose[root_t].0 = rl.inverse() * (pr.inverse() * (f.root - pp) - rlt);
        pose
    };
    // --- planting
    let mut ik = BTreeMap::new();
    let mut events = Vec::new();
    let mut spans_out = Vec::new();
    let mut worst_fk = 0.0f32;
    let mut plans = Vec::new();
    for (set, ends) in [
        (&m.feet, ["ankle_l", "ankle_r"]),
        (&m.hands, ["wrist_l", "wrist_r"]),
    ] {
        if !set.plant {
            continue;
        }
        for end in ends {
            let Some(e) = skel.bone(end) else { continue };
            if src_of[e].is_none()
                || skel.bones[e]
                    .parent
                    .and_then(|p| skel.bones[p].parent)
                    .is_none()
            {
                continue;
            }
            // FK positions in the travel frame
            let world: Vec<(Vec3, Quat)> = frames
                .iter()
                .enumerate()
                .map(|(k, f)| {
                    let w = skel.world(&pose_of(f));
                    let (_, r, t) = w[e].to_scale_rotation_translation();
                    let back = if in_place { vel * time(k) } else { Vec3::ZERO };
                    (t + back, r)
                })
                .collect();
            let low = world.iter().map(|w| w.0.y).fold(f32::INFINITY, f32::min);
            let contact: Vec<bool> = (0..nf)
                .map(|k| {
                    let a = world[k.saturating_sub(1)].0;
                    let b = world[(k + 1).min(nf - 1)].0;
                    let steps = ((k + 1).min(nf - 1) - k.saturating_sub(1)).max(1) as f32;
                    let v = (b - a).length() / (steps * dt);
                    world[k].0.y - low < set.contact_height && v < set.contact_speed
                })
                .collect();
            let mut spans: Vec<(usize, usize, Vec3)> = Vec::new();
            let mut k = 0;
            while k < nf {
                if !contact[k] {
                    k += 1;
                    continue;
                }
                let a = k;
                while k < nf && contact[k] {
                    k += 1;
                }
                let b = k - 1;
                if b > a {
                    let mut mean = Vec3::ZERO;
                    for w in &world[a..=b] {
                        mean += w.0;
                    }
                    mean /= (b - a + 1) as f32;
                    for w in &world[a..=b] {
                        worst_fk = worst_fk.max((w.0 - mean).length());
                    }
                    spans.push((a, b, mean));
                }
            }
            if spans.is_empty() {
                lines.push(format!("{end}: no contact spans found"));
                continue;
            }
            plans.push((end, e, world, spans));
        }
    }
    // A planted foot the leg can't reach (a longer source leg, a knee
    // that can't straighten past its rest bend) lowers the hips instead.
    const RAMP: usize = 3;
    let w_of = |k: usize, spans: &[(usize, usize, Vec3)]| -> (f32, Option<Vec3>) {
        let near = spans
            .iter()
            .map(|&(a, b, p)| {
                let d = if k < a { a - k } else { k.saturating_sub(b) };
                (d, p)
            })
            .min_by_key(|x| x.0);
        match near {
            Some((0, p)) => (1.0, Some(p)),
            Some((d, p)) if d <= RAMP => (1.0 - d as f32 / (RAMP + 1) as f32, Some(p)),
            _ => (0.0, None),
        }
    };
    let mut drop = vec![0.0f32; nf];
    for (_, e, _, spans) in &plans {
        let e = *e;
        let (Some(mid), Some(top)) = (
            skel.bones[e].parent,
            skel.bones[e].parent.and_then(|p| skel.bones[p].parent),
        ) else {
            continue;
        };
        let straight = (head(mid) - head(top)).length() + (head(e) - head(mid)).length();
        let locked = skel.bones[mid]
            .limits
            .as_ref()
            .and_then(|l| l.x)
            .is_some_and(|x| x[0] >= -1e-3);
        let reach = 0.998
            * if locked {
                (head(e) - head(top)).length()
            } else {
                straight
            };
        for (k, f) in frames.iter().enumerate() {
            let (w, Some(a)) = w_of(k, spans) else {
                continue;
            };
            let wm = skel.world(&pose_of(f));
            let back = if in_place { vel * time(k) } else { Vec3::ZERO };
            let h = wm[top].w_axis.truncate() + back;
            let d = a - h;
            let h2 = d.x * d.x + d.z * d.z;
            if h2 >= reach * reach || d.length() <= reach {
                continue;
            }
            let need = (h.y - a.y) - (reach * reach - h2).sqrt();
            drop[k] = drop[k].max(need.max(0.0) * w);
        }
    }
    if drop.iter().any(|d| *d > 0.0) {
        let grown: Vec<f32> = (0..nf)
            .map(|k| {
                (k.saturating_sub(3)..(k + 4).min(nf))
                    .map(|i| drop[i])
                    .fold(0.0, f32::max)
            })
            .collect();
        let smooth: Vec<f32> = (0..nf)
            .map(|k| {
                let r = k.saturating_sub(2)..(k + 3).min(nf);
                let n = r.len() as f32;
                r.map(|i| grown[i]).sum::<f32>() / n
            })
            .collect();
        let most = smooth.iter().copied().fold(0.0, f32::max);
        for (f, d) in frames.iter_mut().zip(&smooth) {
            f.root.y -= d;
        }
        lines.push(format!(
            "hips lowered up to {:.0} mm so the planted feet stay reachable",
            most * 1000.0
        ));
    }
    for (end, e, world, spans) in plans {
        let mut target = Vec::with_capacity(nf);
        let mut weight = Vec::with_capacity(nf);
        let mut rotate = Vec::with_capacity(nf);
        for (k, wk) in world.iter().enumerate() {
            let (w, a) = w_of(k, &spans);
            let back = if in_place { vel * time(k) } else { Vec3::ZERO };
            let p = a.unwrap_or(wk.0) - back;
            let t = time(k);
            target.push((t, [p.x, p.y, p.z, 0.0]));
            weight.push((t, [w, 0.0, 0.0, 0.0]));
            let q = (wk.1 * skel.bones[e].rest.inverse()).normalize();
            rotate.push((t, q.to_array()));
        }
        for &(a, _, _) in &spans {
            let side = end.rsplit('_').next().unwrap_or("");
            let base = if end.starts_with("ankle") {
                "footstep"
            } else {
                "handplant"
            };
            events.push(ClipEvent {
                time: time(a),
                name: format!("{base}_{side}"),
                data: None,
            });
        }
        lines.push(format!(
            "{end}: {} contact span(s): {}",
            spans.len(),
            spans
                .iter()
                .map(|&(a, b, _)| format!("{}-{} s", fmt_s(time(a)), fmt_s(time(b))))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        ik.insert(
            end.to_string(),
            IkDef {
                target: IkTarget::Keys(Channel::Keys {
                    keys: target,
                    interp: Interp::Linear,
                }),
                chain: 2,
                pole: None,
                weight: Some(Channel::Keys {
                    keys: weight,
                    interp: Interp::Linear,
                }),
                rotate: Some(Channel::Keys {
                    keys: rotate,
                    interp: Interp::Linear,
                }),
            },
        );
        spans_out.push((e, spans));
    }
    events.sort_by(|a, b| a.time.total_cmp(&b.time));
    // --- tracks
    let mut tracks = BTreeMap::new();
    for i in 0..nb {
        if src_of[i].is_none() {
            continue;
        }
        let keys: Vec<(f32, [f32; 4])> = frames
            .iter()
            .enumerate()
            .map(|(k, f)| (time(k), f.rots[i].unwrap_or(Quat::IDENTITY).to_array()))
            .collect();
        let mut tr = TrackDef {
            rotate: Some(Channel::Keys {
                keys,
                interp: m.interp,
            }),
            ..TrackDef::default()
        };
        if i == root_t {
            let poses: Vec<crate::anim::Pose> = frames.iter().map(pose_of).collect();
            tr.translate = Some(Channel::Keys {
                keys: poses
                    .iter()
                    .enumerate()
                    .map(|(k, p)| {
                        let v = p[i].0;
                        (time(k), [v.x, v.y, v.z, 0.0])
                    })
                    .collect(),
                interp: m.interp,
            });
        }
        tracks.insert(skel.bones[i].name.clone(), tr);
    }
    let mut out = def.clone();
    out.mocap = None;
    out.duration = dur;
    out.tracks = tracks;
    out.ik = ik;
    out.events.extend(events);
    if let Some(ax) = &axes {
        out.root_motion = Some(RootMotionDef {
            bone: skel.bones[root_t].name.clone(),
            axes: ax.clone(),
            yaw: false,
        });
    }
    // --- report
    let unmapped: Vec<&str> = (0..src.joints.len())
        .filter(|j| !src_of.contains(&Some(*j)))
        .map(|j| src.joints[j].name.as_str())
        .collect();
    let mut mapped: Vec<String> = (0..nb)
        .filter_map(|i| {
            src_of[i].map(|j| format!("{}<-{}", skel.bones[i].name, src.joints[j].name))
        })
        .collect();
    mapped.truncate(64);
    let mut head_lines = vec![
        format!(
            "{ctx} onto `{}`: {} s, {nf} frames at {} fps (source {} frames at {:.0} fps, {} s used{}{})",
            skel.name,
            fmt_s(dur),
            m.fps,
            src.frames.len(),
            1.0 / src.frame_time,
            fmt_s(t1 - t0),
            if m.speed != 1.0 {
                format!(", speed {}", m.speed)
            } else {
                String::new()
            },
            if m.mirror { ", mirrored" } else { "" }
        ),
        format!(
            "file units: {unit_name} (source height {:.3} -> {:.2} m); root scale {:.4} ({by})",
            s_height,
            s_height * unit,
            scale,
        ),
        format!(
            "mapped {}/{} joints: {}",
            mapped.len(),
            src.joints.len(),
            mapped.join(", ")
        ),
    ];
    if !unmapped.is_empty() {
        head_lines.push(format!(
            "unmapped joints (their motion rides the mapped bones below them): {}",
            unmapped.join(", ")
        ));
    }
    let speed = (vel.x * vel.x + vel.z * vel.z).sqrt();
    head_lines.push(if in_place {
        format!(
            "travel [{:.2}, {:.2}] m removed (in place; {:.2} m/s, the clip's speed [{:.2}, {:.2}]){}",
            travel.x,
            travel.z,
            speed,
            vel.x,
            vel.z,
            if yaw != 0.0 {
                format!("; turned {yaw:.0}°")
            } else {
                String::new()
            }
        )
    } else {
        format!(
            "root motion `{}` on `{}`: travel [{:.2}, {:.2}] m ({:.2} m/s)",
            axes.as_deref().unwrap_or(""),
            skel.bones[root_t].name,
            travel.x,
            travel.z,
            speed
        )
    });
    if !spans_out.is_empty() {
        head_lines.push(format!(
            "unplanted slide (FK) up to {:.0} mm during contacts",
            worst_fk * 1000.0
        ));
    }
    head_lines.extend(src.warnings.iter().cloned());
    head_lines.extend(lines);
    Ok((
        out,
        Lowered {
            lines: head_lines,
            speed: [vel.x, vel.z],
            in_place,
            spans: spans_out,
            dt,
            frames: nf,
        },
    ))
}

/// After the clip is built: the speed, and the planted slide measured on
/// the real (IK-solved) clip, into its report.
pub fn finish(clip: &mut crate::anim::Clip, skel: &Skeleton, low: &Lowered) {
    clip.speed = Some(low.speed);
    let mut lines = low.lines.clone();
    let vel = Vec3::new(low.speed[0], 0.0, low.speed[1]);
    let mut worst = 0.0f32;
    let mut worst_at = String::new();
    for (e, spans) in &low.spans {
        for &(a, b, p) in spans {
            for k in a..=b {
                let t = (k as f32 * low.dt).min(clip.duration - 1e-4);
                let w = skel.world(&clip.sample(skel, t));
                let back = if low.in_place { vel * t } else { Vec3::ZERO };
                let d = (w[*e].w_axis.truncate() + back - p).length();
                if d > worst {
                    worst = d;
                    worst_at = format!(" (`{}` at {} s)", skel.bones[*e].name, fmt_s(t));
                }
            }
        }
    }
    if !low.spans.is_empty() {
        lines.push(format!(
            "planted: worst foot slide {:.1} mm{worst_at}",
            worst * 1000.0
        ));
    }
    clip.report = Some(lines.join("\n"));
}
