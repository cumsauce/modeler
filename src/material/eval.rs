//! Evaluates a material graph over a sample set.
//!
//! Nodes run in dependency *levels*: every node in a level depends only on
//! earlier levels, so a level's nodes run in parallel (rayon), and each
//! kernel is itself data-parallel over the samples. Results are memoized in a
//! process-wide cache keyed by a Merkle hash (the node's resolved JSON plus
//! the keys of the nodes it reads, plus the sample domain), so a tweak re-runs
//! only that node and what reads it. All of it is deterministic: kernels are
//! pure functions of their inputs and the hashes are seeded integers.

use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use rayon::prelude::*;
use serde_json::Value;

use super::node::{self, Buf, Cx, Field, In, Node, Output};
use crate::params::{self, Params};

/// Where a graph is evaluated: one sample per texel of a `w` × `h` grid.
#[derive(Debug, Clone)]
pub struct Samples {
    pub w: usize,
    pub h: usize,
    /// Surface coordinates in meters (vec2).
    pub uv: Arc<Buf>,
    /// Object-space position in meters (vec3).
    pub pos: Arc<Buf>,
    /// Object-space unit normal (vec3).
    pub nrm: Arc<Buf>,
    /// Texels that lie on the surface (object domain); `None` = all.
    pub valid: Option<Arc<Vec<bool>>>,
    /// Tile size in meters when the grid wraps (tile domain), else 0.
    pub period: f32,
    /// Meters per sample (for anti-aliasing and neighbourhood radii).
    pub texel: f32,
    /// Identifies the domain in cache keys.
    pub key: u64,
    /// The part's mesh (object domain on a part), for mesh-aware nodes.
    pub mesh: Option<Arc<super::meshvox::MeshVox>>,
}

impl Samples {
    /// A tileable `n` × `n` grid over a `tile` × `tile` meter square: uv runs
    /// 0..tile along +x (columns) and +y (rows, top row first, so the image
    /// reads like the uv layout glTF samples); pos = (u, v, 0), normal +z.
    pub fn tile(n: usize, tile: f32) -> Samples {
        Samples::tile_shifted(n, tile, 0)
    }

    /// [`Samples::tile`] with every coordinate moved by `shift` texels on
    /// both axes (the tileable check bakes a half-tile shift and compares).
    pub fn tile_shifted(n: usize, tile: f32, shift: usize) -> Samples {
        let n = n.max(1);
        let t = tile.max(1e-6) / n as f32;
        let k = shift as f32;
        let uv: Vec<[f32; 2]> = (0..n * n)
            .map(|i| {
                [
                    ((i % n) as f32 + 0.5 + k) * t,
                    ((i / n) as f32 + 0.5 + k) * t,
                ]
            })
            .collect();
        let pos = uv.iter().map(|p| [p[0], p[1], 0.0]).collect();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        ("tile", n, tile.to_bits(), shift).hash(&mut h);
        Samples {
            w: n,
            h: n,
            uv: Arc::new(Buf::V2(uv)),
            pos: Arc::new(Buf::V3(pos)),
            nrm: Arc::new(Buf::V3(vec![[0.0, 0.0, 1.0]; n * n])),
            valid: None,
            period: tile,
            texel: t,
            key: h.finish(),
            mesh: None,
        }
    }

    /// The surface of a sphere of `radius` meters centred at the origin as
    /// an equirectangular grid (`2n` × `n`, top row = north pole): pos and
    /// normal per texel, uv = (longitude, colatitude) arc lengths in meters.
    /// The object-domain preview, before part atlases exist.
    pub fn sphere(n: usize, radius: f32) -> Samples {
        let (w, h) = ((2 * n).max(2), n.max(1));
        let r = radius.max(1e-6);
        let mut uv = Vec::with_capacity(w * h);
        let mut pos = Vec::with_capacity(w * h);
        let mut nrm = Vec::with_capacity(w * h);
        for y in 0..h {
            let th = (y as f32 + 0.5) / h as f32 * std::f32::consts::PI;
            for x in 0..w {
                let ph = (x as f32 + 0.5) / w as f32 * std::f32::consts::TAU;
                let d = [th.sin() * ph.sin(), th.cos(), th.sin() * ph.cos()];
                uv.push([ph * r, th * r]);
                pos.push(d.map(|c| c * r));
                nrm.push(d);
            }
        }
        let mut hs = std::collections::hash_map::DefaultHasher::new();
        ("sphere", n, r.to_bits()).hash(&mut hs);
        Samples {
            w,
            h,
            uv: Arc::new(Buf::V2(uv)),
            pos: Arc::new(Buf::V3(pos)),
            nrm: Arc::new(Buf::V3(nrm)),
            valid: None,
            period: 0.0,
            texel: std::f32::consts::PI * r / h as f32,
            key: hs.finish(),
            mesh: None,
        }
    }

    pub fn len(&self) -> usize {
        self.w * self.h
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The sample `dx`, `dy` texels from `i`: wrapping on a tile, clamped to
    /// the grid and to valid texels otherwise (`None` off the surface).
    #[inline]
    pub fn neighbour(&self, i: usize, dx: i64, dy: i64) -> Option<usize> {
        let (w, h) = (self.w as i64, self.h as i64);
        let (x, y) = ((i % self.w) as i64 + dx, (i / self.w) as i64 + dy);
        let (x, y) = if self.period > 0.0 {
            (x.rem_euclid(w), y.rem_euclid(h))
        } else if x < 0 || y < 0 || x >= w || y >= h {
            return None;
        } else {
            (x, y)
        };
        let j = (y * w + x) as usize;
        match &self.valid {
            Some(v) if !v[j] => None,
            _ => Some(j),
        }
    }
}

/// One parsed node of a graph.
#[derive(Debug, Clone)]
pub struct Parsed {
    pub name: String,
    pub ty: String,
    pub node: Node,
    /// The resolved body (params substituted), for cache keys.
    pub body: Value,
    /// Node names this one reads.
    pub reads: Vec<String>,
}

/// Parses and checks a graph: names unique, types known, fields valid,
/// every reference names a node, no cycles. Returns nodes in list order.
pub fn parse(nodes: &[Value], params: &Params) -> Result<Vec<Parsed>, String> {
    let mut out: Vec<Parsed> = Vec::new();
    for (k, raw) in nodes.iter().enumerate() {
        let v = params::resolve(raw, params).map_err(|e| format!("node #{k}: {e}"))?;
        let Value::Object(mut m) = v else {
            return Err(format!(
                "node #{k}: expected an object with \"type\" and \"name\""
            ));
        };
        let ty = match m.remove("type") {
            Some(Value::String(s)) => s,
            _ => {
                return Err(format!(
                    "node #{k}: missing \"type\" (one of: {})",
                    node::NODE_TYPES.join(", ")
                ));
            }
        };
        let name = match m.remove("name") {
            Some(Value::String(s)) if !s.is_empty() && !s.contains('.') && !s.starts_with('#') => s,
            Some(Value::String(s)) => {
                return Err(format!(
                    "node #{k} ({ty}): name `{s}`: use a non-empty name without '.' or a leading '#'"
                ));
            }
            _ => return Err(format!("node #{k} ({ty}): missing \"name\"")),
        };
        if out.iter().any(|p| p.name == name) {
            return Err(format!(
                "node #{k} ({ty} \"{name}\"): another node is already named `{name}`"
            ));
        }
        let body = Value::Object(m);
        let ctx = format!("node #{k} ({ty} \"{name}\")");
        let node = node::parse_body(&ty, &body, &ctx)?;
        let mut reads: Vec<String> = node::refs(&body).into_iter().map(|(n, _)| n).collect();
        reads.sort();
        reads.dedup();
        out.push(Parsed {
            name,
            ty,
            node,
            body,
            reads,
        });
    }
    for p in &out {
        for r in &p.reads {
            if r == &p.name {
                return Err(format!("node `{}` reads itself", p.name));
            }
            if !out.iter().any(|q| &q.name == r) {
                let names: Vec<&str> = out.iter().map(|q| q.name.as_str()).collect();
                return Err(format!(
                    "node `{}` ({}): reads `{r}`, which is not a node (nodes: {}; colours are \"#rrggbb\")",
                    p.name,
                    p.ty,
                    names.join(", ")
                ));
            }
        }
    }
    levels(&out)?;
    Ok(out)
}

/// Dependency levels: level 0 reads nothing; each node sits one level after
/// the deepest node it reads.
pub fn levels(nodes: &[Parsed]) -> Result<Vec<Vec<usize>>, String> {
    let idx: HashMap<&str, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, p)| (p.name.as_str(), i))
        .collect();
    let mut depth: Vec<Option<usize>> = vec![None; nodes.len()];
    fn visit(
        i: usize,
        nodes: &[Parsed],
        idx: &HashMap<&str, usize>,
        depth: &mut Vec<Option<usize>>,
        stack: &mut Vec<usize>,
    ) -> Result<usize, String> {
        if let Some(d) = depth[i] {
            return Ok(d);
        }
        if stack.contains(&i) {
            let names: Vec<&str> = stack.iter().map(|&j| nodes[j].name.as_str()).collect();
            return Err(format!(
                "nodes form a cycle: {} -> {}",
                names.join(" -> "),
                nodes[i].name
            ));
        }
        stack.push(i);
        let mut d = 0;
        for r in &nodes[i].reads {
            if let Some(&j) = idx.get(r.as_str()) {
                d = d.max(visit(j, nodes, idx, depth, stack)? + 1);
            }
        }
        stack.pop();
        depth[i] = Some(d);
        Ok(d)
    }
    let mut stack = Vec::new();
    for i in 0..nodes.len() {
        visit(i, nodes, &idx, &mut depth, &mut stack)?;
    }
    let max = depth.iter().flatten().max().copied().unwrap_or(0);
    let mut lv = vec![Vec::new(); if nodes.is_empty() { 0 } else { max + 1 }];
    for (i, d) in depth.iter().enumerate() {
        lv[d.unwrap_or(0)].push(i);
    }
    Ok(lv)
}

/// A node's outputs: (port, buffer), the first being the default.
pub type Ports = Arc<Vec<(&'static str, Arc<Buf>)>>;

/// The result of evaluating a graph.
#[derive(Debug, Clone)]
pub struct Evaluated {
    pub nodes: Vec<Parsed>,
    pub ports: HashMap<String, Ports>,
    /// Nodes actually computed (cache misses) by this evaluation.
    pub runs: usize,
}

impl Evaluated {
    /// The output node, if the graph has one.
    /// The main output: the last `output` node no other output layers in.
    pub fn output(&self) -> Option<&Output> {
        let layered: Vec<&str> = self
            .nodes
            .iter()
            .filter_map(|p| match &p.node {
                Node::Output(o) => Some(o.over.iter().map(|l| l.output.as_str())),
                _ => None,
            })
            .flatten()
            .collect();
        self.nodes.iter().rev().find_map(|p| match &p.node {
            Node::Output(o) if !layered.contains(&p.name.as_str()) => Some(o),
            _ => None,
        })
    }

    /// An `output` node by name (a layer).
    pub fn output_named(&self, name: &str) -> Option<&Output> {
        self.nodes.iter().find_map(|p| match &p.node {
            Node::Output(o) if p.name == name => Some(o),
            _ => None,
        })
    }

    /// Reads an input against these results (for channel assembly).
    pub fn field(&self, i: &In) -> Result<Field, String> {
        resolve(&self.ports, i)
    }
}

fn resolve(ports: &HashMap<String, Ports>, i: &In) -> Result<Field, String> {
    match i {
        In::Num(x) => Ok(Field::F(*x)),
        In::Vec(v) if v.is_empty() || v.len() > 4 => Err(format!("{v:?}: give 1 to 4 numbers")),
        In::Vec(v) => Ok(Field::V(v.iter().take(3).copied().collect())),
        In::Name(s) if s.starts_with('#') => node::hex(s)
            .map(Field::V)
            .ok_or_else(|| format!("`{s}`: colours are \"#rgb\" or \"#rrggbb\"")),
        In::Name(s) => {
            let (n, p) = s.split_once('.').unwrap_or((s, ""));
            let Some(outs) = ports.get(n) else {
                return Err(format!("`{n}` is not a node"));
            };
            if outs.is_empty() {
                return Err(format!("`{n}` has no outputs to read"));
            }
            if p.is_empty() {
                return Ok(Field::Buf(outs[0].1.clone()));
            }
            match outs.iter().find(|(q, _)| *q == p) {
                Some((_, b)) => Ok(Field::Buf(b.clone())),
                None => {
                    let names: Vec<&str> = outs.iter().map(|(q, _)| *q).collect();
                    Err(format!(
                        "`{n}` has no port `{p}` (ports: {})",
                        names.join(", ")
                    ))
                }
            }
        }
    }
}

/// Cap on cached node results; dropped wholesale when exceeded.
const CACHE_MAX: usize = 512;

static CACHE: Mutex<Option<HashMap<u64, Ports>>> = Mutex::new(None);

fn cache_get(k: u64) -> Option<Ports> {
    CACHE.lock().ok()?.as_ref()?.get(&k).cloned()
}

fn cache_put(k: u64, v: Ports) {
    if let Ok(mut c) = CACHE.lock() {
        let m = c.get_or_insert_with(HashMap::new);
        if m.len() >= CACHE_MAX {
            m.clear();
        }
        m.insert(k, v);
    }
}

/// Drops every cached node result (tests, memory pressure).
pub fn clear_cache() {
    if let Ok(mut c) = CACHE.lock() {
        *c = None;
    }
}

/// Evaluates `nodes` with `params` over `s`; `cached: false` bypasses the
/// memo cache (used to prove a cached result matches a fresh one).
pub fn evaluate(
    nodes: &[Value],
    params: &Params,
    s: &Samples,
    cached: bool,
) -> Result<Evaluated, String> {
    let parsed = parse(nodes, params)?;
    let lv = levels(&parsed)?;
    let mut keys: HashMap<String, u64> = HashMap::new();
    let mut ports: HashMap<String, Ports> = HashMap::new();
    let mut runs = 0usize;
    for level in lv {
        // Keys first (they read the previous levels' keys), then evaluate the
        // level's nodes in parallel against the finished levels.
        let jobs: Vec<(usize, u64)> = level
            .iter()
            .map(|&i| {
                let p = &parsed[i];
                let mut h = std::collections::hash_map::DefaultHasher::new();
                (p.ty.as_str(), p.body.to_string(), s.key).hash(&mut h);
                for r in &p.reads {
                    keys.get(r).copied().unwrap_or(0).hash(&mut h);
                }
                (i, h.finish())
            })
            .collect();
        let done: Vec<Result<(usize, u64, Ports, bool), String>> = jobs
            .par_iter()
            .map(|&(i, k)| {
                let p = &parsed[i];
                if cached && let Some(hit) = cache_get(k) {
                    return Ok((i, k, hit, false));
                }
                let get = |x: &In| resolve(&ports, x);
                let cx = Cx { s, get: &get };
                let outs = p
                    .node
                    .eval(&cx)
                    .map_err(|e| format!("node `{}` ({}): {e}", p.name, p.ty))?;
                for (port, b) in &outs {
                    let n = match b {
                        Buf::F(v) => v.len(),
                        Buf::V2(v) => v.len(),
                        Buf::V3(v) => v.len(),
                        Buf::Id(v) => v.len(),
                    };
                    if n != s.len() {
                        return Err(format!(
                            "node `{}` ({}): port `{port}` made {n} samples, expected {}",
                            p.name,
                            p.ty,
                            s.len()
                        ));
                    }
                }
                let outs: Ports =
                    Arc::new(outs.into_iter().map(|(n, b)| (n, Arc::new(b))).collect());
                if cached {
                    cache_put(k, outs.clone());
                }
                Ok((i, k, outs, true))
            })
            .collect();
        for d in done {
            let (i, k, outs, ran) = d?;
            runs += usize::from(ran);
            keys.insert(parsed[i].name.clone(), k);
            ports.insert(parsed[i].name.clone(), outs);
        }
    }
    Ok(Evaluated {
        nodes: parsed,
        ports,
        runs,
    })
}

/// Node names in the graph mapped to their types (for summaries).
pub fn types(nodes: &[Parsed]) -> BTreeMap<&str, &str> {
    nodes
        .iter()
        .map(|p| (p.name.as_str(), p.ty.as_str()))
        .collect()
}
