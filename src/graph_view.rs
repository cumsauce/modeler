//! A high-level view of the op graph for the preview's node panel: one box per
//! op that produces geometry, edges from each input's producer to its reader,
//! in-place edits (material, hide, transform, rename, move) folded in as
//! badges, plus a small spring layout that lets new boxes drop in and settle.

use crate::{Model, Op, graph};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

#[derive(Debug, Clone)]
pub struct ViewNode {
    /// Stable identity across syncs: `kind:name`.
    pub key: String,
    pub kind: &'static str,
    pub name: String,
    /// Part names this box writes (the parts to highlight when clicked).
    pub outputs: Vec<String>,
    pub badges: Vec<&'static str>,
    /// Every output is hidden (an intermediate, e.g. a cutter).
    pub dimmed: bool,
    /// Longest path from a source.
    pub depth: usize,
    /// Changes when the op or its badges change (drives the glow).
    pub sig: u64,
}

#[derive(Debug, Clone, Default)]
pub struct GraphView {
    pub nodes: Vec<ViewNode>,
    /// (from, to) node indices.
    pub edges: Vec<(usize, usize)>,
    /// Moves over every part (`*`): shown once, not as a badge on every box.
    pub global_moves: Vec<String>,
}

fn in_place(op: &Op) -> bool {
    match op {
        Op::Move { .. } | Op::Rename { .. } => true,
        Op::Material { .. } | Op::Visibility { .. } | Op::Transform { .. } => {
            graph::inputs(op).first() == Some(&graph::output(op))
        }
        _ => false,
    }
}

fn badge(op: &Op) -> &'static str {
    match op {
        Op::Material { .. } => "color",
        Op::Visibility { visible: false, .. } => "hidden",
        Op::Visibility { .. } => "shown",
        Op::Transform { .. } => "placed",
        Op::Rename { .. } => "renamed",
        _ => "moved",
    }
}

fn matches(pattern: &str, name: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(pre) => name.starts_with(pre),
        None => pattern == name,
    }
}

/// Builds the view from a model's op list.
pub fn build(model: &Model) -> GraphView {
    let ops = &model.ops;
    let mut view = GraphView::default();
    // Which node currently owns each part name (renames move ownership).
    let mut owner: HashMap<String, usize> = HashMap::new();
    let mut extra: Vec<Vec<u64>> = Vec::new();
    for op in ops {
        if in_place(op) {
            if let Op::Move { name, parts, .. } = op
                && parts.iter().any(|p| p == "*")
            {
                view.global_moves.push(name.clone());
                continue;
            }
            let targets: Vec<usize> = match op {
                Op::Move { parts, .. } => {
                    let mut t: Vec<usize> = owner
                        .iter()
                        .filter(|(n, _)| parts.iter().any(|p| matches(p, n)))
                        .map(|(_, &i)| i)
                        .collect();
                    t.sort_unstable();
                    t.dedup();
                    t
                }
                _ => graph::inputs(op)
                    .first()
                    .and_then(|i| owner.get(*i))
                    .copied()
                    .into_iter()
                    .collect(),
            };
            let mut h = std::collections::hash_map::DefaultHasher::new();
            format!("{op:?}").hash(&mut h);
            for &t in &targets {
                let b = badge(op);
                if !view.nodes[t].badges.contains(&b) {
                    view.nodes[t].badges.push(b);
                }
                extra[t].push(h.finish());
            }
            if let Op::Rename { name, input } = op
                && let Some(i) = owner.remove(input)
            {
                owner.insert(name.clone(), i);
                if let Some(o) = view.nodes[i].outputs.iter_mut().find(|o| *o == input) {
                    o.clone_from(name);
                }
            }
            continue;
        }
        let idx = view.nodes.len();
        let mut depth = 0;
        for input in graph::inputs(op) {
            if let Some(&from) = owner.get(input) {
                if !view.edges.contains(&(from, idx)) {
                    view.edges.push((from, idx));
                }
                depth = depth.max(view.nodes[from].depth + 1);
            }
        }
        let mut outputs = graph::outputs(op);
        // A radial array with its source writes the source as copy 0.
        if let Op::RadialArray {
            input,
            include_source: true,
            ..
        } = op
        {
            outputs.insert(0, input.to_string());
        }
        for o in &outputs {
            owner.insert(o.clone(), idx);
        }
        let mut h = std::collections::hash_map::DefaultHasher::new();
        format!("{op:?}").hash(&mut h);
        view.nodes.push(ViewNode {
            key: format!("{}:{}", graph::kind(op), graph::output(op)),
            kind: graph::kind(op),
            name: graph::output(op).to_string(),
            outputs,
            badges: Vec::new(),
            dimmed: false,
            depth,
            sig: h.finish(),
        });
        extra.push(Vec::new());
    }
    for (n, e) in view.nodes.iter_mut().zip(extra) {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (n.sig, e).hash(&mut h);
        n.sig = h.finish();
        // Hidden or consumed by a later op: nothing of it shows in the scene.
        n.dimmed = n
            .outputs
            .iter()
            .all(|o| !model.parts.iter().any(|p| &p.name == o && p.visible));
    }
    view
}

/// Spring layout state: positions persist by node key across syncs.
#[derive(Debug, Default)]
pub struct Layout {
    pub pos: HashMap<String, [f32; 2]>,
    vel: HashMap<String, [f32; 2]>,
    /// Seconds of glow left per key (set when a node is new or changed).
    pub glow: HashMap<String, f32>,
    sigs: HashMap<String, u64>,
}

pub const COL: f32 = 200.0;

impl Layout {
    /// Adopts a new view: keeps known positions, drops new nodes in near their
    /// inputs (from above, so they visibly fall into place), glows changes.
    pub fn sync(&mut self, view: &GraphView) {
        let first = self.pos.is_empty();
        let keys: Vec<&String> = view.nodes.iter().map(|n| &n.key).collect();
        self.pos.retain(|k, _| keys.contains(&k));
        self.vel.retain(|k, _| keys.contains(&k));
        for (i, n) in view.nodes.iter().enumerate() {
            if !self.pos.contains_key(&n.key) {
                let parents: Vec<[f32; 2]> = view
                    .edges
                    .iter()
                    .filter(|e| e.1 == i)
                    .filter_map(|e| self.pos.get(&view.nodes[e.0].key).copied())
                    .collect();
                let y = if parents.is_empty() {
                    i as f32 * 50.0
                } else {
                    parents.iter().map(|p| p[1]).sum::<f32>() / parents.len() as f32
                };
                let drop = if first { 0.0 } else { -120.0 };
                self.pos
                    .insert(n.key.clone(), [n.depth as f32 * COL, y + drop]);
                self.vel.insert(n.key.clone(), [0.0, 0.0]);
            }
            if !first && self.sigs.get(&n.key) != Some(&n.sig) {
                self.glow.insert(n.key.clone(), 1.5);
            }
        }
        self.sigs = view.nodes.iter().map(|n| (n.key.clone(), n.sig)).collect();
        self.glow.retain(|k, _| keys.contains(&k));
    }

    /// One physics step: columns by depth, springs along edges, repulsion
    /// between boxes in the same column band, damping. Returns kinetic energy.
    pub fn step(&mut self, view: &GraphView, dt: f32) -> f32 {
        let dt = dt.min(1.0 / 30.0);
        let n = view.nodes.len();
        let p: Vec<[f32; 2]> = view
            .nodes
            .iter()
            .map(|v| self.pos.get(&v.key).copied().unwrap_or([0.0, 0.0]))
            .collect();
        let mut f = vec![[0.0f32; 2]; n];
        for (i, v) in view.nodes.iter().enumerate() {
            f[i][0] += (v.depth as f32 * COL - p[i][0]) * 30.0;
        }
        for &(a, b) in &view.edges {
            let dy = p[b][1] - p[a][1];
            f[a][1] += dy * 4.0;
            f[b][1] -= dy * 4.0;
        }
        for i in 0..n {
            for j in (i + 1)..n {
                let dx = p[j][0] - p[i][0];
                let dy = p[j][1] - p[i][1];
                if dx.abs() > COL * 0.8 {
                    continue;
                }
                let gap = 46.0 - dy.abs();
                if gap > 0.0 {
                    let s = if dy.abs() < 0.01 {
                        if i < j { 1.0 } else { -1.0 }
                    } else {
                        dy.signum()
                    };
                    f[i][1] -= s * gap * 40.0;
                    f[j][1] += s * gap * 40.0;
                }
            }
        }
        let mut energy = 0.0;
        for (i, v) in view.nodes.iter().enumerate() {
            let vel = self.vel.entry(v.key.clone()).or_insert([0.0, 0.0]);
            for (k, fk) in f[i].iter().enumerate() {
                vel[k] = (vel[k] + fk * dt) * (1.0 - 6.0 * dt).max(0.0);
            }
            energy += vel[0] * vel[0] + vel[1] * vel[1];
            if let Some(pos) = self.pos.get_mut(&v.key) {
                pos[0] += vel[0] * dt;
                pos[1] += vel[1] * dt;
            }
        }
        for g in self.glow.values_mut() {
            *g -= dt;
        }
        self.glow.retain(|_, g| *g > 0.0);
        energy
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::Session;

    fn model(items: serde_json::Value) -> Model {
        let mut s = Session::default();
        let serde_json::Value::Array(items) = items else {
            panic!("array");
        };
        s.apply_values_as(None, items, true)
            .unwrap_or_else(|e| panic!("{e}"));
        s.model
    }

    #[test]
    fn in_place_edits_become_badges_and_edges_follow_inputs() {
        let m = model(serde_json::json!([
            {"type": "primitive", "name": "a", "kind": "box", "size": 1},
            {"type": "primitive", "name": "cut", "kind": "box", "size": 0.5},
            {"type": "material", "name": "a", "color": "#f00"},
            {"type": "hide_part", "part": "cut"},
            {"type": "boolean", "name": "b", "op": "difference", "a": "a", "b": "cut"},
            {"type": "mirror", "name": "m", "input": "b", "axis": "x", "offset": [2, 0, 0]},
            {"type": "move", "name": "lean", "parts": ["*"], "axis": [1, 0, 0], "degrees": 5}
        ]));
        let v = build(&m);
        let names: Vec<&str> = v.nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["a", "cut", "b", "m"]);
        assert!(v.nodes[0].badges.contains(&"color"));
        assert!(v.nodes[1].dimmed && v.nodes[1].badges.contains(&"hidden"));
        assert!(v.nodes.iter().all(|n| !n.badges.contains(&"moved")));
        assert_eq!(v.global_moves, ["lean"]);
        assert_eq!(v.nodes[3].depth, 2);
        assert!(
            v.edges.contains(&(0, 2)) && v.edges.contains(&(1, 2)) && v.edges.contains(&(2, 3))
        );
    }

    #[test]
    fn layout_settles_and_keeps_boxes_apart() {
        let m = model(serde_json::json!([
            {"type": "primitive", "name": "a", "kind": "box", "size": 1},
            {"type": "primitive", "name": "b", "kind": "box", "size": 1},
            {"type": "primitive", "name": "c", "kind": "box", "size": 1},
            {"type": "group", "name": "g", "inputs": ["a", "b", "c"]}
        ]));
        let v = build(&m);
        let mut l = Layout::default();
        l.sync(&v);
        let mut e = f32::MAX;
        for _ in 0..600 {
            e = l.step(&v, 1.0 / 60.0);
        }
        assert!(e < 1.0, "energy {e}");
        let ys: Vec<f32> = ["primitive:a", "primitive:b", "primitive:c"]
            .iter()
            .map(|k| l.pos[*k][1])
            .collect();
        for i in 0..3 {
            for j in (i + 1)..3 {
                assert!((ys[i] - ys[j]).abs() > 40.0, "{ys:?}");
            }
        }
        assert!((l.pos["group:g"][0] - COL).abs() < 2.0);
    }
}
