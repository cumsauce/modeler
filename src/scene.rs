//! Level export: a map document becomes a `.bsn` scene (jackdaw's format,
//! Bevy 0.20 syntax), a glTF per prefab it places, one glTF of its own
//! geometry, and a heights file per terrain.
//!
//! - **Entities.** Each `prefab` instance is an entity with a `GltfSource`
//!   pointing at that prefab's glTF (exported once, re-exported when the
//!   prefab file is newer), each `marker` an entity at its spot, and the
//!   document's own visible parts (ground, roads, walls) one more entity
//!   with the area glTF. Every entity has a stable `SceneNodeId` (a hash of
//!   the scene and entity names) and the file lists them sorted by name, so
//!   re-exports diff cleanly and merges between agents mostly just work.
//! - **Components** come from a vocabulary the game declares in
//!   `components.json` beside the catalog (`<project>/.modeler/`), so a typo
//!   fails when the item is applied, not when the game loads the scene:
//!
//!   ```json
//!   {"header": ["// SALT: ..."],
//!    "components": {
//!      "Place": {"type": "friendslop_world::places::Place",
//!                "fields": {"name": "string"}},
//!      "Claim": {"type": "friendslop_world::footprints::Claim",
//!                "fields": {"radius": "f32"}},
//!      "Rigged": {"type": "friendslop::props::Rigged"}}}
//!   ```
//!
//!   Field types: `string`, `f32`, `i32`, `u32`, `bool`, `vec2`, `vec3`.
//!   `header` lines go at the top of every scene file (a project's required
//!   comment). Fields left out keep the component's default (BSN patches).
//! - **Heights.** Each `terrain` op writes `<scene>.<terrain>.heights`
//!   (see [`write_heights`]): a little-endian grid the game reads instead of
//!   evaluating the scene.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use bevy::math::{Affine3A, Quat, Vec3};
use serde::Deserialize;

use crate::{Model, ModelerError, Op};

/// jackdaw's version stamp (its loader migrates older text by it).
pub const STAMP: &str = "// jackdaw 0.20.2 | bevy 0.20";
const GLTF_SOURCE: &str = "jackdaw_scene_types::types::GltfSource";
const NODE_ID: &str = "jackdaw_scene_types::node_id::SceneNodeId";
const TRANSFORM: &str = "bevy_transform::components::transform::Transform";
const CHILDREN: &str = "bevy_ecs::hierarchy::Children";

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Vocabulary {
    /// Lines written at the top of every scene file.
    #[serde(default)]
    pub header: Vec<String>,
    pub components: BTreeMap<String, ComponentDef>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentDef {
    /// The Rust type path the game registers (`my_game::places::Place`).
    #[serde(rename = "type")]
    pub type_path: String,
    /// Field name -> type (`string`, `f32`, `i32`, `u32`, `bool`, `vec2`,
    /// `vec3`). Empty for a marker component.
    #[serde(default)]
    pub fields: BTreeMap<String, String>,
    /// What it means, for agents (`describe_op("components")`).
    #[serde(default)]
    pub doc: String,
}

const FIELD_TYPES: [&str; 7] = ["string", "f32", "i32", "u32", "bool", "vec2", "vec3"];

/// Where the game's vocabulary lives: `components.json` beside the catalog
/// folders (`<project>/.modeler/components.json`).
pub fn vocabulary_path() -> Result<PathBuf, ModelerError> {
    let dir = crate::catalog::catalog_dir()?;
    Ok(dir
        .parent()
        .map(|p| p.join("components.json"))
        .unwrap_or_else(|| dir.join("components.json")))
}

/// The vocabulary, or `None` when the project declares none.
pub fn load_vocabulary() -> Result<Option<Vocabulary>, String> {
    let path = vocabulary_path().map_err(|e| e.to_string())?;
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    let v: Vocabulary =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    for (name, c) in &v.components {
        if c.type_path.split("::").count() < 2 {
            return Err(format!(
                "{}: `{name}`: type `{}` should be a full Rust path (crate::module::Type)",
                path.display(),
                c.type_path
            ));
        }
        for (f, t) in &c.fields {
            if !FIELD_TYPES.contains(&t.as_str()) {
                return Err(format!(
                    "{}: `{name}.{f}`: unknown field type `{t}` (one of {})",
                    path.display(),
                    FIELD_TYPES.join(", ")
                ));
            }
        }
    }
    Ok(Some(v))
}

/// Checks an item's `components` against the vocabulary: every component
/// declared, every field known and of its type.
pub fn check_components(
    components: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    if components.is_empty() {
        return Ok(());
    }
    let path = vocabulary_path().map_err(|e| e.to_string())?;
    let Some(v) = load_vocabulary()? else {
        return Err(format!(
            "components: the project declares none (no {}); the game lists the components a map may carry there",
            path.display()
        ));
    };
    for (name, value) in components {
        let Some(def) = v.components.get(name) else {
            return Err(format!(
                "components: unknown `{name}` (declared: {})",
                v.components.keys().cloned().collect::<Vec<_>>().join(", ")
            ));
        };
        let fields = match value {
            serde_json::Value::Object(m) => m.clone(),
            serde_json::Value::Null | serde_json::Value::Bool(true) => Default::default(),
            other => {
                return Err(format!(
                    "components.{name}: an object of fields (or {{}} / true for none), got {other}"
                ));
            }
        };
        for (f, val) in &fields {
            let Some(t) = def.fields.get(f) else {
                return Err(format!(
                    "components.{name}: unknown field `{f}` (fields: {})",
                    if def.fields.is_empty() {
                        "none".to_string()
                    } else {
                        def.fields.keys().cloned().collect::<Vec<_>>().join(", ")
                    }
                ));
            };
            emit_value(t, val).map_err(|e| format!("components.{name}.{f}: {e}"))?;
        }
    }
    Ok(())
}

/// A field value as BSN text.
fn emit_value(t: &str, v: &serde_json::Value) -> Result<String, String> {
    let num = || {
        v.as_f64()
            .ok_or_else(|| format!("expected a number ({t}), got {v}"))
    };
    let vec = |n: usize| -> Result<Vec<f64>, String> {
        let a = v
            .as_array()
            .filter(|a| a.len() == n)
            .ok_or_else(|| format!("expected [{}] ({t}), got {v}", vec!["n"; n].join(", ")))?;
        a.iter()
            .map(|x| {
                x.as_f64()
                    .ok_or_else(|| format!("expected numbers, got {v}"))
            })
            .collect()
    };
    Ok(match t {
        "string" => {
            let s = v
                .as_str()
                .ok_or_else(|| format!("expected a string, got {v}"))?;
            serde_json::to_string(s).map_err(|e| e.to_string())?
        }
        "f32" => float(num()? as f32),
        "i32" => {
            let x = num()?;
            if x.fract() != 0.0 || x < i32::MIN as f64 || x > i32::MAX as f64 {
                return Err(format!("expected a whole number, got {v}"));
            }
            format!("{}", x as i64)
        }
        "u32" => {
            let x = num()?;
            if x.fract() != 0.0 || x < 0.0 || x > u32::MAX as f64 {
                return Err(format!("expected a whole number >= 0, got {v}"));
            }
            format!("{}", x as u64)
        }
        "bool" => v
            .as_bool()
            .ok_or_else(|| format!("expected true or false, got {v}"))?
            .to_string(),
        "vec2" => {
            let a = vec(2)?;
            format!(
                "glam::Vec2 {{ x: {}, y: {} }}",
                float(a[0] as f32),
                float(a[1] as f32)
            )
        }
        "vec3" => {
            let a = vec(3)?;
            vec3(Vec3::new(a[0] as f32, a[1] as f32, a[2] as f32))
        }
        other => return Err(format!("unknown field type `{other}`")),
    })
}

/// A float that always reads as one (`1.0`, not `1`).
fn float(x: f32) -> String {
    let x = if x == 0.0 { 0.0 } else { x };
    let s = format!("{x}");
    if s.contains(['.', 'e', 'E']) || !x.is_finite() {
        s
    } else {
        format!("{s}.0")
    }
}

fn vec3(v: Vec3) -> String {
    format!(
        "glam::Vec3 {{ x: {}, y: {}, z: {} }}",
        float(v.x),
        float(v.y),
        float(v.z)
    )
}

/// A stable 64-bit id: FNV-1a of the scene and entity names.
pub fn node_id(scene: &str, entity: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in scene.bytes().chain(*b"/").chain(entity.bytes()) {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    // Keep it clear of the top bit: some readers parse ids as i64.
    h >> 1
}

/// `#Name`, quoted when it isn't a plain identifier.
fn entity_name(n: &str) -> String {
    let plain = n
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if plain {
        format!("#{n}")
    } else {
        format!(
            "#{}",
            serde_json::to_string(n).unwrap_or_else(|_| "\"?\"".into())
        )
    }
}

/// One scene entity.
#[derive(Debug, Clone)]
pub struct Entity {
    pub name: String,
    pub transform: Affine3A,
    /// glTF path relative to the asset folder.
    pub gltf: Option<String>,
    /// (type path, BSN field lines or empty for a unit component).
    pub components: Vec<(String, Vec<(String, String)>)>,
}

fn transform_text(t: &Affine3A) -> String {
    let (s, r, p) = t.to_scale_rotation_translation();
    let mut fields = Vec::new();
    if p.length() > 1e-6 {
        fields.push(format!("translation: {}", vec3(p)));
    }
    if r.angle_between(Quat::IDENTITY) > 1e-5 {
        let r = r.normalize();
        fields.push(format!(
            "rotation: glam::Quat {{ x: {}, y: {}, z: {}, w: {} }}",
            float(r.x),
            float(r.y),
            float(r.z),
            float(r.w)
        ));
    }
    if (s - Vec3::ONE).abs().max_element() > 1e-5 {
        fields.push(format!("scale: {}", vec3(s)));
    }
    if fields.is_empty() {
        TRANSFORM.to_string()
    } else {
        format!(
            "{TRANSFORM} {{\n{}\n    }}",
            fields
                .iter()
                .map(|f| format!("        {f},"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    }
}

/// The scene text: stamp, header lines, then every entity as a root child,
/// sorted by name.
pub fn emit(scene: &str, header: &[String], entities: &[Entity]) -> String {
    let mut es: Vec<&Entity> = entities.iter().collect();
    es.sort_by(|a, b| a.name.cmp(&b.name));
    let mut out = String::new();
    out += STAMP;
    out.push('\n');
    for h in header {
        let h = h.trim_end();
        if h.trim_start().starts_with("//") {
            out += h;
        } else {
            out += "// ";
            out += h;
        }
        out.push('\n');
    }
    let _ = writeln!(
        out,
        "// Written by the modeler (export_model to .bsn): edit the map document, not this file."
    );
    out += &format!("{CHILDREN} [\n");
    for (i, e) in es.iter().enumerate() {
        if i > 0 {
            out += "    --\n";
        }
        let _ = writeln!(out, "    {}", entity_name(&e.name));
        let _ = writeln!(out, "    {NODE_ID}({})", node_id(scene, &e.name));
        let _ = writeln!(out, "    {}", transform_text(&e.transform));
        if let Some(g) = &e.gltf {
            let _ = writeln!(
                out,
                "    {GLTF_SOURCE} {{\n        path: {},\n        scene_index: 0,\n    }}",
                serde_json::to_string(g).unwrap_or_default()
            );
        }
        for (ty, fields) in &e.components {
            if fields.is_empty() {
                let _ = writeln!(out, "    {ty}");
            } else {
                let _ = writeln!(out, "    {ty} {{");
                for (k, v) in fields {
                    let _ = writeln!(out, "        {k}: {v},");
                }
                let _ = writeln!(out, "    }}");
            }
        }
    }
    out += "]\n";
    out
}

/// An item's `components` as (type path, fields) in BSN text.
fn resolve_components(
    vocab: Option<&Vocabulary>,
    components: &serde_json::Map<String, serde_json::Value>,
) -> Result<Vec<(String, Vec<(String, String)>)>, String> {
    if components.is_empty() {
        return Ok(Vec::new());
    }
    check_components(components)?;
    let Some(v) = vocab else {
        return Err("components: the project declares none".into());
    };
    let mut out = Vec::new();
    for (name, value) in components {
        let Some(def) = v.components.get(name) else {
            continue;
        };
        let mut fields = Vec::new();
        if let serde_json::Value::Object(m) = value {
            for (f, val) in m {
                let t = def.fields.get(f).map(String::as_str).unwrap_or("string");
                fields.push((f.clone(), emit_value(t, val)?));
            }
        }
        out.push((def.type_path.clone(), fields));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn affine(t: [f32; 3], r: [f32; 4], s: [f32; 3]) -> Affine3A {
    Affine3A::from_scale_rotation_translation(Vec3::from(s), crate::quat_of(r), Vec3::from(t))
}

/// Where a prefab instance ended up: its first visible part's transform
/// against that part's own in the prefab (so moves and places count), else
/// the item's transform.
fn instance_transform(
    model: &Model,
    name: &str,
    source: Option<&Model>,
    fallback: Affine3A,
) -> Affine3A {
    let prefix = format!("{name}.");
    let Some(src) = source else {
        return fallback;
    };
    for p in model.parts.iter().filter(|p| p.visible) {
        let Some(local) = p.name.strip_prefix(&prefix) else {
            continue;
        };
        if let Some(sp) = src.parts.iter().find(|q| q.name == local) {
            let world = affine(p.translation, p.rotation, p.scale);
            let own = affine(sp.translation, sp.rotation, sp.scale);
            return world * own.inverse();
        }
    }
    fallback
}

/// The asset folder a path sits in (the nearest `assets` ancestor, else the
/// project's), for paths relative to it.
fn asset_root(scene: &Path) -> PathBuf {
    for a in scene.ancestors() {
        if a.file_name().is_some_and(|n| n == "assets") {
            return a.to_path_buf();
        }
    }
    crate::catalog::project_root()
        .map(|r| r.join("assets"))
        .unwrap_or_else(|| scene.parent().map(Path::to_path_buf).unwrap_or_default())
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Options for [`export_scene`].
#[derive(Debug, Clone)]
pub struct SceneOptions {
    /// Where prefab glTFs go, relative to the asset folder.
    pub prefab_dir: String,
    /// Re-export every prefab glTF even when it looks current.
    pub refresh: bool,
    /// The export optimisation for every glTF (default `game`).
    pub optimize: crate::export_opt::Options,
}

impl Default for SceneOptions {
    fn default() -> Self {
        Self {
            prefab_dir: "models/prefabs".into(),
            refresh: false,
            optimize: crate::export_opt::Options::game(),
        }
    }
}

fn mtime(p: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// Writes the map: `<path>.bsn`, the area glTF beside it, a glTF per placed
/// prefab (when missing or older than the prefab), and a heights file per
/// terrain. Returns a report.
pub fn export_scene(
    model: &Model,
    path: &Path,
    opts: &SceneOptions,
) -> Result<String, ModelerError> {
    let vocab = load_vocabulary().map_err(ModelerError::from)?;
    let root = asset_root(path);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("scene")
        .to_string();
    let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
    std::fs::create_dir_all(&dir).map_err(|e| ModelerError::Io(e.to_string()))?;
    let mut entities = Vec::new();
    let mut report = Vec::new();
    let mut owned: Vec<String> = Vec::new();
    // Prefab instances, by reference.
    let mut exported: BTreeMap<String, String> = BTreeMap::new();
    for op in &model.ops {
        let Op::Prefab {
            name,
            prefab,
            translation,
            rotation,
            scale,
            components,
            ..
        } = op
        else {
            continue;
        };
        owned.push(format!("{name}."));
        let source = crate::catalog::load_prefab(prefab).ok();
        let gltf = match exported.get(prefab) {
            Some(g) => g.clone(),
            None => {
                let file = root
                    .join(&opts.prefab_dir)
                    .join(format!("{}.gltf", crate::catalog::sanitize(prefab)));
                let src_file = crate::catalog::catalog_dir()
                    .ok()
                    .map(|d| d.join(format!("{}.json", crate::catalog::sanitize(prefab))));
                let stale = opts.refresh
                    || match (mtime(&file), src_file.as_deref().and_then(mtime)) {
                        (Some(out), Some(src)) => out < src,
                        (None, _) => true,
                        _ => false,
                    };
                if stale {
                    let Some(src) = &source else {
                        return Err(format!(
                            "prefab `{prefab}` (instance `{name}`) is not in the catalog"
                        )
                        .into());
                    };
                    if let Some(d) = file.parent() {
                        std::fs::create_dir_all(d).map_err(|e| ModelerError::Io(e.to_string()))?;
                    }
                    crate::gltf::export_gltf_with(src, &file, &opts.optimize)?;
                    report.push(format!(
                        "exported prefab `{prefab}` -> {}",
                        rel(&root, &file)
                    ));
                }
                let r = rel(&root, &file);
                exported.insert(prefab.clone(), r.clone());
                r
            }
        };
        let fallback = affine(
            *translation,
            rotation.unwrap_or([0.0, 0.0, 0.0, 1.0]),
            scale.unwrap_or([1.0; 3]),
        );
        entities.push(Entity {
            name: name.clone(),
            transform: instance_transform(model, name, source.as_ref(), fallback),
            gltf: Some(gltf),
            components: resolve_components(vocab.as_ref(), components)
                .map_err(|e| format!("instance `{name}`: {e}"))?,
        });
    }
    // Markers.
    for m in crate::game::markers(model)? {
        // Markers inside a prefab ride in that prefab's glTF.
        if m.name.contains('.') {
            continue;
        }
        let comps = model
            .ops
            .iter()
            .find_map(|o| match o {
                Op::Marker {
                    name, components, ..
                } if *name == m.name => Some(components.clone()),
                _ => None,
            })
            .unwrap_or_default();
        entities.push(Entity {
            name: m.name.clone(),
            transform: Affine3A::from_rotation_translation(m.rotation, m.position),
            gltf: None,
            components: resolve_components(vocab.as_ref(), &comps)
                .map_err(|e| format!("marker `{}`: {e}", m.name))?,
        });
    }
    // The document's own geometry as one glTF.
    let mut area = model.clone_without_history();
    let mut kept = 0;
    let mut parts = Vec::with_capacity(area.parts.len());
    for p in &area.parts {
        let mine = p.visible && !owned.iter().any(|o| p.name.starts_with(o.as_str()));
        if mine {
            kept += 1;
            parts.push(p.clone());
        } else if p.visible {
            let mut q = (**p).clone();
            q.visible = false;
            parts.push(std::sync::Arc::new(q));
        } else {
            parts.push(p.clone());
        }
    }
    area.parts = parts;
    if kept > 0 {
        let file = dir.join(format!("{stem}.gltf"));
        let r = crate::gltf::export_gltf_with(&area, &file, &opts.optimize)?;
        report.push(format!(
            "exported the map's own geometry ({kept} part(s)) -> {}: {}",
            rel(&root, &file),
            r.lines().next().unwrap_or("")
        ));
        entities.push(Entity {
            name: format!("{stem}_ground"),
            transform: Affine3A::IDENTITY,
            gltf: Some(rel(&root, &file)),
            components: Vec::new(),
        });
    }
    // Terrain heights.
    for op in &model.ops {
        if let Op::Terrain { name, def } = op {
            let file = dir.join(format!("{stem}.{name}.heights"));
            let (nx, nz) = write_heights(def, &file)?;
            report.push(format!(
                "terrain `{name}` heights {nx} x {nz} -> {}",
                rel(&root, &file)
            ));
        }
    }
    let header = vocab.as_ref().map(|v| v.header.clone()).unwrap_or_default();
    let text = emit(&stem, &header, &entities);
    crate::catalog::atomic_write(path, text.as_bytes())?;
    let n_inst = entities.iter().filter(|e| e.gltf.is_some()).count();
    Ok(format!(
        "scene `{}`: {} entities ({} with a glTF), {} prefab(s) referenced\n{}",
        rel(&root, path),
        entities.len(),
        n_inst,
        exported.len(),
        report.join("\n")
    ))
}

/// Magic at the start of a heights file.
pub const HEIGHTS_MAGIC: &[u8; 4] = b"MHT1";

/// Writes a terrain's top as a grid the game can read without the scene:
///
/// ```text
/// "MHT1"                       4 bytes
/// nx, nz: u32                  samples along x and z (cells + 1)
/// x0, z0: f32                  world position of sample (0, 0)
/// cell: f32                    meters between samples
/// h_min, h_max: f32            world heights that 0 and 65535 stand for
/// heights: nx * nz u16         row-major, z rows of x samples
/// mask: nx * nz u8             bit 0: on a path, bit 1: on a flattened pad
/// ```
///
/// All little-endian. Height = h_min + v / 65535 * (h_max - h_min); sample
/// between grid points bilinearly. Returns (nx, nz).
pub fn write_heights(
    t: &crate::terrain::Terrain,
    path: &Path,
) -> Result<(usize, usize), ModelerError> {
    if t.rotation
        .is_some_and(|r| crate::quat_of(r).angle_between(Quat::IDENTITY) > 1e-4)
    {
        return Err(
            "terrain heights: a rotated terrain can't be written as a grid; place it unrotated"
                .into(),
        );
    }
    let (cx, cz) = t.cells();
    let (nx, nz) = (cx + 1, cz + 1);
    let [w, d] = t.size;
    let x0 = -w * 0.5;
    let z0 = -d * 0.5;
    let step_x = w / cx.max(1) as f32;
    let step_z = d / cz.max(1) as f32;
    let cell = step_x;
    let [tx, ty, tz] = t.translation;
    let mut h = Vec::with_capacity(nx * nz);
    let mut mask = Vec::with_capacity(nx * nz);
    for j in 0..nz {
        for i in 0..nx {
            let (x, z) = (x0 + i as f32 * step_x, z0 + j as f32 * step_z);
            h.push(t.height_at(x, z) + ty);
            let mut m = 0u8;
            if t.paths.iter().any(|p| {
                p.points
                    .windows(2)
                    .any(|s| seg_dist([x, z], s[0], s[1]) <= p.width * 0.5)
            }) {
                m |= 1;
            }
            if t.flatten.iter().any(|p| match p.size {
                Some([pw, pd]) => {
                    (x - p.at[0]).abs() <= pw * 0.5 && (z - p.at[1]).abs() <= pd * 0.5
                }
                None => ((x - p.at[0]).powi(2) + (z - p.at[1]).powi(2)).sqrt() <= p.radius,
            }) {
                m |= 2;
            }
            mask.push(m);
        }
    }
    let lo = h.iter().copied().fold(f32::INFINITY, f32::min);
    let hi = h
        .iter()
        .copied()
        .fold(f32::NEG_INFINITY, f32::max)
        .max(lo + 1e-3);
    let mut out = Vec::with_capacity(32 + nx * nz * 3);
    out.extend_from_slice(HEIGHTS_MAGIC);
    out.extend_from_slice(&(nx as u32).to_le_bytes());
    out.extend_from_slice(&(nz as u32).to_le_bytes());
    for f in [x0 + tx, z0 + tz, cell, lo, hi] {
        out.extend_from_slice(&f.to_le_bytes());
    }
    for v in &h {
        let q = ((v - lo) / (hi - lo) * 65535.0).round().clamp(0.0, 65535.0) as u16;
        out.extend_from_slice(&q.to_le_bytes());
    }
    out.extend_from_slice(&mask);
    crate::catalog::atomic_write(path, &out)?;
    Ok((nx, nz))
}

fn seg_dist(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let (ax, az, bx, bz) = (a[0], a[1], b[0], b[1]);
    let (dx, dz) = (bx - ax, bz - az);
    let l2 = dx * dx + dz * dz;
    let t = if l2 > 0.0 {
        (((p[0] - ax) * dx + (p[1] - az) * dz) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (qx, qz) = (ax + t * dx - p[0], az + t * dz - p[1]);
    (qx * qx + qz * qz).sqrt()
}

/// Reads a heights file back: (nx, nz, x0, z0, cell, heights, mask).
pub type Heights = (usize, usize, f32, f32, f32, Vec<f32>, Vec<u8>);

pub fn read_heights(bytes: &[u8]) -> Option<Heights> {
    if bytes.len() < 32 || &bytes[..4] != HEIGHTS_MAGIC {
        return None;
    }
    let u = |o: usize| u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    let f = |o: usize| f32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    let (nx, nz) = (u(4) as usize, u(8) as usize);
    let (x0, z0, cell, lo, hi) = (f(12), f(16), f(20), f(24), f(28));
    let n = nx * nz;
    if bytes.len() < 32 + n * 3 {
        return None;
    }
    let h = (0..n)
        .map(|i| {
            let v = u16::from_le_bytes([bytes[32 + i * 2], bytes[33 + i * 2]]);
            lo + v as f32 / 65535.0 * (hi - lo)
        })
        .collect();
    Some((
        nx,
        nz,
        x0,
        z0,
        cell,
        h,
        bytes[32 + n * 2..32 + n * 3].to_vec(),
    ))
}
