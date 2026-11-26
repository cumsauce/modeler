//! A map as a Godot 4 text scene (`.tscn`), written by
//! [`crate::scene::export_scene`] with [`crate::scene::SceneFormat::Godot`]
//! from the same entities as the `.bsn` scene:
//!
//! - every prefab instance is an instanced `PackedScene` (its glTF, which
//!   Godot imports itself), the map's own geometry one more, and every
//!   marker a `Marker3D` with `metadata/kind` (and `metadata/size`);
//! - components go into `metadata/components` as a dictionary by vocabulary
//!   name (vec2 / vec3 fields as `Vector2` / `Vector3`, lists as
//!   `PackedStringArray`), a variant into `metadata/variant`;
//! - colliders become physics nodes, since Godot's glTF importer ignores
//!   `extras.collider`: a `StaticBody3D` (`AnimatableBody3D` for kinematic,
//!   `Area3D` for sensors) per entity holding a `CollisionShape3D` per
//!   collider: `BoxShape3D`, `ConvexPolygonShape3D` (hull), and one
//!   `ConcavePolygonShape3D` for the mesh colliders. A prefab's shapes are
//!   shared sub-resources, its body a child of each instance. Dynamic
//!   bodies are left out (a `RigidBody3D` has to own the mesh it moves) and
//!   reported.
//!
//! Paths are `res://` paths from the Godot project root: the nearest folder
//! above the scene with a `project.godot`, else the asset folder.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bevy::math::{Affine3A, Quat, Vec3};

use crate::scene::{Entity, Vocabulary, float};
use crate::{Model, ModelerError};

/// What [`emit`] needs.
pub struct Scene<'a> {
    pub stem: &'a str,
    pub header: &'a [String],
    pub entities: &'a [Entity],
    pub vocab: Option<&'a Vocabulary>,
    /// The map document (for its own colliders).
    pub map: &'a Model,
    /// Part-name prefixes (`inst.`) that belong to prefab instances.
    pub owned: &'a [String],
    /// Each placed prefab's document.
    pub sources: &'a BTreeMap<String, Arc<Model>>,
    /// The asset folder entity glTF paths are relative to.
    pub asset_root: &'a Path,
    /// The folder `res://` stands for.
    pub res_root: &'a Path,
}

/// The scene text and report lines.
pub struct Written {
    pub text: String,
    pub notes: Vec<String>,
}

/// The Godot project root for a scene written to `scene`: the nearest
/// ancestor holding `project.godot`, else the asset folder.
pub fn res_root(scene: &Path, asset_root: &Path) -> PathBuf {
    let start = scene.parent().unwrap_or(scene);
    for a in start.ancestors() {
        if a.join("project.godot").is_file() {
            return a.to_path_buf();
        }
    }
    asset_root.to_path_buf()
}

fn res_path(res_root: &Path, abs: &Path) -> Result<String, ModelerError> {
    let Ok(r) = abs.strip_prefix(res_root) else {
        return Err(format!(
            "{} is outside the Godot project {} (export the scene inside it)",
            abs.display(),
            res_root.display()
        )
        .into());
    };
    Ok(format!("res://{}", r.to_string_lossy().replace('\\', "/")))
}

/// A node name Godot accepts (no `. : @ / " %`).
pub fn node_name(n: &str) -> String {
    let s: String = n
        .chars()
        .map(|c| if ".:@/\"%".contains(c) { '_' } else { c })
        .collect();
    if s.is_empty() { "_".into() } else { s }
}

fn quote(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

fn vector3(v: Vec3) -> String {
    format!("Vector3({}, {}, {})", float(v.x), float(v.y), float(v.z))
}

/// `Transform3D(basis rows..., origin)`, or `None` for the identity.
pub fn transform(t: &Affine3A) -> Option<String> {
    let m = t.matrix3;
    let o = Vec3::from(t.translation);
    if (m - bevy::math::Mat3A::IDENTITY).abs_diff_eq(bevy::math::Mat3A::ZERO, 1e-6)
        && o.length() < 1e-6
    {
        return None;
    }
    let mut v = Vec::with_capacity(12);
    for i in 0..3 {
        for j in 0..3 {
            v.push(float(m.col(j)[i]));
        }
    }
    v.extend([o.x, o.y, o.z].map(float));
    Some(format!("Transform3D({})", v.join(", ")))
}

/// A component field's value as a Godot literal, by its vocabulary type.
fn value(t: Option<&str>, v: &serde_json::Value) -> String {
    use serde_json::Value as J;
    let nums = |a: &Vec<J>| -> Vec<String> {
        a.iter()
            .map(|x| float(x.as_f64().unwrap_or(0.0) as f32))
            .collect()
    };
    match (t, v) {
        (Some("f32"), J::Number(n)) => float(n.as_f64().unwrap_or(0.0) as f32),
        (Some("vec2"), J::Array(a)) if a.len() == 2 => format!("Vector2({})", nums(a).join(", ")),
        (Some("vec3"), J::Array(a)) if a.len() == 3 => format!("Vector3({})", nums(a).join(", ")),
        (Some("strings" | "refs"), J::Array(a)) => format!(
            "PackedStringArray({})",
            a.iter()
                .map(|x| quote(x.as_str().unwrap_or_default()))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        (_, J::Null) => "null".into(),
        (_, J::Bool(b)) => b.to_string(),
        (_, J::Number(n)) => {
            if n.is_f64() {
                float(n.as_f64().unwrap_or(0.0) as f32)
            } else {
                n.to_string()
            }
        }
        (_, J::String(s)) => quote(s),
        (_, J::Array(a)) => format!(
            "[{}]",
            a.iter()
                .map(|x| value(None, x))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        (_, J::Object(m)) => format!(
            "{{{}}}",
            m.iter()
                .map(|(k, x)| format!("{}: {}", quote(k), value(None, x)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn components(
    vocab: Option<&Vocabulary>,
    raw: &serde_json::Map<String, serde_json::Value>,
) -> String {
    let mut out = Vec::new();
    for (name, fields) in raw {
        let def = vocab.and_then(|v| v.components.get(name));
        let body = match fields.as_object() {
            Some(m) => m
                .iter()
                .map(|(f, x)| {
                    let t = def.and_then(|d| d.fields.get(f)).map(String::as_str);
                    format!("{}: {}", quote(f), value(t, x))
                })
                .collect::<Vec<_>>()
                .join(", "),
            None => String::new(),
        };
        out.push(format!("{}: {{{body}}}", quote(name)));
    }
    format!("{{{}}}", out.join(", "))
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Body {
    Static,
    Kinematic,
    Sensor,
}

impl Body {
    fn node(self) -> (&'static str, &'static str) {
        match self {
            Body::Static => ("collision", "StaticBody3D"),
            Body::Kinematic => ("collision_kinematic", "AnimatableBody3D"),
            Body::Sensor => ("sensor", "Area3D"),
        }
    }
}

struct ShapeNode {
    name: String,
    transform: Affine3A,
    sub: String,
}

/// Sub-resources in file order.
#[derive(Default)]
struct Subs {
    blocks: Vec<String>,
}

impl Subs {
    fn add(&mut self, ty: &str, key: &str, body: String) -> String {
        let id = format!(
            "{ty}_{}_{}",
            node_name(key).replace(' ', "_"),
            self.blocks.len() + 1
        );
        self.blocks.push(format!(
            "[sub_resource type=\"{ty}\" id=\"{id}\"]\n{body}\n"
        ));
        id
    }
}

/// The physics shapes of `model`'s colliders (skipping parts under `skip`
/// prefixes), in `model`'s frame, by body. Returns the count of dynamic
/// colliders left out.
fn shapes(
    model: &Model,
    skip: &[String],
    key: &str,
    subs: &mut Subs,
) -> Result<(BTreeMap<Body, Vec<ShapeNode>>, usize), ModelerError> {
    let mut out: BTreeMap<Body, Vec<ShapeNode>> = BTreeMap::new();
    let mut soups: BTreeMap<Body, Vec<Vec3>> = BTreeMap::new();
    let mut dynamic = 0;
    for c in crate::game::colliders(model)? {
        if skip.iter().any(|s| c.part.starts_with(s.as_str())) {
            continue;
        }
        let body = if c.sensor {
            Body::Sensor
        } else {
            match c.body.as_str() {
                "static" => Body::Static,
                "kinematic" => Body::Kinematic,
                _ => {
                    dynamic += 1;
                    continue;
                }
            }
        };
        match c.shape.as_str() {
            "box" => {
                let sub = subs.add(
                    "BoxShape3D",
                    key,
                    format!("size = {}", vector3(c.half_extents * 2.0)),
                );
                out.entry(body).or_default().push(ShapeNode {
                    name: c.part.clone(),
                    transform: Affine3A::from_rotation_translation(c.rotation, c.center),
                    sub,
                });
            }
            "hull" => {
                let pts: Vec<String> = c.points.iter().flat_map(|p| p.map(float)).collect();
                let sub = subs.add(
                    "ConvexPolygonShape3D",
                    key,
                    format!("points = PackedVector3Array({})", pts.join(", ")),
                );
                out.entry(body).or_default().push(ShapeNode {
                    name: c.part.clone(),
                    transform: Affine3A::from_rotation_translation(Quat::IDENTITY, c.center),
                    sub,
                });
            }
            "mesh" => {
                let Some(p) = model.parts.iter().find(|p| p.name == c.part) else {
                    continue;
                };
                let w = crate::game::world_verts(p);
                let soup = soups.entry(body).or_default();
                for t in p.indices.as_chunks::<3>().0 {
                    for &i in t {
                        if let Some(v) = w.get(i as usize) {
                            soup.push(*v);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    for (body, soup) in soups {
        if soup.is_empty() {
            continue;
        }
        let data: Vec<String> = soup.iter().flat_map(|v| v.to_array().map(float)).collect();
        let sub = subs.add(
            "ConcavePolygonShape3D",
            key,
            format!("data = PackedVector3Array({})", data.join(", ")),
        );
        out.entry(body).or_default().push(ShapeNode {
            name: "mesh".into(),
            transform: Affine3A::IDENTITY,
            sub,
        });
    }
    Ok((out, dynamic))
}

/// The body and shape nodes under `parent` (a node path from the root).
fn body_nodes(
    out: &mut String,
    parent: &str,
    prefix: &str,
    shapes: &BTreeMap<Body, Vec<ShapeNode>>,
) {
    for (body, list) in shapes {
        let (name, ty) = body.node();
        let name = format!("{prefix}{name}");
        let _ = writeln!(
            out,
            "[node name=\"{name}\" type=\"{ty}\" parent=\"{parent}\"]\n"
        );
        let path = if parent == "." {
            name.clone()
        } else {
            format!("{parent}/{name}")
        };
        let mut used = std::collections::BTreeSet::new();
        for s in list {
            let base = node_name(&s.name);
            let mut n = base.clone();
            let mut k = 2;
            while !used.insert(n.clone()) {
                n = format!("{base}_{k}");
                k += 1;
            }
            let _ = writeln!(
                out,
                "[node name=\"{n}\" type=\"CollisionShape3D\" parent=\"{path}\"]"
            );
            if let Some(t) = transform(&s.transform) {
                let _ = writeln!(out, "transform = {t}");
            }
            let _ = writeln!(out, "shape = SubResource(\"{}\")\n", s.sub);
        }
    }
}

/// The `.tscn` text for a map's entities.
pub fn emit(s: &Scene) -> Result<Written, ModelerError> {
    let mut notes = Vec::new();
    let mut es: Vec<&Entity> = s.entities.iter().collect();
    es.sort_by(|a, b| a.name.cmp(&b.name));
    // External scenes: one per glTF, in path order.
    let mut ext: BTreeMap<String, String> = BTreeMap::new();
    for e in &es {
        if let Some(g) = &e.gltf {
            let p = res_path(s.res_root, &s.asset_root.join(g))?;
            ext.entry(p).or_default();
        }
    }
    for (i, id) in ext.values_mut().enumerate() {
        *id = format!("{}_gltf", i + 1);
    }
    // Shapes: each prefab's once, then the map's own.
    let mut subs = Subs::default();
    let mut dynamic = 0;
    let mut prefab_shapes = BTreeMap::new();
    for (name, src) in s.sources {
        let (sh, d) = shapes(src, &[], name, &mut subs)?;
        dynamic += d * es
            .iter()
            .filter(|e| e.prefab.as_deref() == Some(name))
            .count();
        prefab_shapes.insert(name.clone(), sh);
    }
    let (own, d) = shapes(s.map, s.owned, s.stem, &mut subs)?;
    dynamic += d;
    if dynamic > 0 {
        notes.push(format!(
            "godot: {dynamic} dynamic collider(s) left out: a RigidBody3D has to own the mesh it moves, so set those up in the game"
        ));
    }

    let mut out = String::from("[gd_scene format=3]\n\n");
    for h in s.header {
        let h = h.trim();
        let h = h.strip_prefix("//").map(str::trim_start).unwrap_or(h);
        let _ = writeln!(out, "; {h}");
    }
    out += "; Written by the modeler (export_model to .tscn): edit the map document, not this file.\n\n";
    for (path, id) in &ext {
        let _ = writeln!(
            out,
            "[ext_resource type=\"PackedScene\" path={} id=\"{id}\"]",
            quote(path)
        );
    }
    if !ext.is_empty() {
        out.push('\n');
    }
    for b in &subs.blocks {
        out += b;
        out.push('\n');
    }
    let root = node_name(s.stem);
    let _ = writeln!(out, "[node name={} type=\"Node3D\"]", quote(&root));
    let _ = writeln!(
        out,
        "metadata/map_json = {}\n",
        quote(&format!("{}.map.json", s.stem))
    );
    let mut used = std::collections::BTreeSet::new();
    for e in &es {
        let base = node_name(&e.name);
        let mut name = base.clone();
        let mut k = 2;
        while !used.insert(name.clone()) {
            name = format!("{base}_{k}");
            k += 1;
        }
        match &e.gltf {
            Some(g) => {
                let p = res_path(s.res_root, &s.asset_root.join(g))?;
                let id = ext.get(&p).cloned().unwrap_or_default();
                let _ = writeln!(
                    out,
                    "[node name={} parent=\".\" instance=ExtResource(\"{id}\")]",
                    quote(&name)
                );
            }
            None => {
                let _ = writeln!(
                    out,
                    "[node name={} type=\"Marker3D\" parent=\".\"]",
                    quote(&name)
                );
            }
        }
        if let Some(t) = transform(&e.transform) {
            let _ = writeln!(out, "transform = {t}");
        }
        if name != e.name {
            let _ = writeln!(out, "metadata/modeler_name = {}", quote(&e.name));
        }
        if let Some(k) = &e.kind {
            let _ = writeln!(out, "metadata/kind = {}", quote(k));
        }
        if let Some(sz) = e.size {
            let _ = writeln!(out, "metadata/size = {}", vector3(Vec3::from(sz)));
        }
        if let Some(v) = &e.variant {
            let _ = writeln!(out, "metadata/variant = {}", quote(v));
        }
        if !e.raw.is_empty() {
            let _ = writeln!(out, "metadata/components = {}", components(s.vocab, &e.raw));
        }
        out.push('\n');
        if let Some(sh) = e.prefab.as_ref().and_then(|p| prefab_shapes.get(p)) {
            body_nodes(&mut out, &name, "", sh);
        }
    }
    body_nodes(&mut out, ".", &format!("{root}_"), &own);
    while out.ends_with("\n\n") {
        out.pop();
    }
    Ok(Written { text: out, notes })
}
