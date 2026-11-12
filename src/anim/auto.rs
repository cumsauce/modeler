//! Auto-rigs: `skeleton` `from` an lsystem (bones along its branches, exact
//! weights for its wood and leaves) or a skin op (a bone per chain point).

use bevy::math::Vec3;

use super::SkeletonDef;
use crate::Part;
use crate::lsystem::Branches;

/// A generated bone before the skeleton is built: name, parent name, head,
/// tail, radius.
pub type RawBone = (String, Option<String>, Vec3, Option<Vec3>, Option<f32>);

/// Exact weights of an lsystem rig: per chain, per chain point, up to two
/// (bone, weight) pairs. Bone indices are into the built skeleton.
#[derive(Clone, Debug, PartialEq)]
pub struct AutoMap {
    /// The lsystem op the rig came from (its wood part has this name, its
    /// leaves `<from>_leaves`).
    pub from: String,
    pub point: Vec<Vec<[(u16, f32); 2]>>,
}

/// Bones along an lsystem's branches; returns the bones and the per-point
/// weights by generated bone index (remapped once the skeleton is built).
pub fn from_lsystem(
    from: &str,
    def: &SkeletonDef,
    part: &Part,
    br: &Branches,
) -> Result<(Vec<RawBone>, Vec<Vec<[(usize, f32); 2]>>), String> {
    let bl = def.bone_length.unwrap_or(0.5);
    let max_depth = def.max_depth.unwrap_or(3);
    let budget = def.max_bones.unwrap_or(128) as usize;
    let rot = crate::quat_of(part.rotation);
    let scale = Vec3::from(part.scale);
    let world = |p: Vec3| rot * (p * scale) + Vec3::from(part.translation);
    let rscale = scale.max_element();
    let chains = &br.chains;
    let n = chains.len();
    let pts: Vec<Vec<(Vec3, f32)>> = chains
        .iter()
        .map(|c| c.pts.iter().map(|(p, r)| (world(*p), r * rscale)).collect())
        .collect();
    let arcs: Vec<Vec<f32>> = pts
        .iter()
        .map(|p| {
            let mut s = vec![0.0f32];
            for w in p.windows(2) {
                let last = s.last().copied().unwrap_or(0.0);
                s.push(last + w[0].0.distance(w[1].0));
            }
            s
        })
        .collect();
    let len: Vec<f32> = arcs
        .iter()
        .map(|a| a.last().copied().unwrap_or(0.0))
        .collect();
    let nb: Vec<usize> = len
        .iter()
        .map(|l| ((l / bl).round() as usize).max(1))
        .collect();
    // Which chains get bones: the thickest levels first, within budget.
    let mut order: Vec<usize> = (0..n).filter(|&c| chains[c].depth <= max_depth).collect();
    order.sort_by(|&a, &b| {
        chains[a]
            .depth
            .cmp(&chains[b].depth)
            .then(len[b].total_cmp(&len[a]))
    });
    let mut selected = vec![false; n];
    let mut used = 0usize;
    for c in order {
        let parent_ok = chains[c]
            .parent
            .is_none_or(|(p, _)| selected.get(p as usize).copied().unwrap_or(false));
        if parent_ok && used + nb[c] <= budget {
            selected[c] = true;
            used += nb[c];
        }
    }
    if used == 0 {
        return Err(format!(
            "`{from}`: no branch fits {budget} bones at bone_length {bl}: raise bone_length or max_bones"
        ));
    }
    let mut bones: Vec<RawBone> = Vec::new();
    let mut point: Vec<Vec<[(usize, f32); 2]>> = Vec::with_capacity(n);
    let mut primary: Vec<Vec<usize>> = Vec::with_capacity(n);
    for c in 0..n {
        let ch = &chains[c];
        let attach = ch
            .parent
            .and_then(|(pc, pp)| primary.get(pc as usize)?.get(pp as usize).copied());
        let m = pts[c].len();
        if !selected[c] {
            // Rides the bone it grows from, rigidly.
            let b = attach.unwrap_or(0);
            point.push(vec![[(b, 1.0), (b, 0.0)]; m]);
            primary.push(vec![b; m]);
            continue;
        }
        let k = nb[c];
        let seg = (len[c] / k as f32).max(1e-6);
        let at = |s: f32| -> (Vec3, f32) {
            let a = &arcs[c];
            let i = a.partition_point(|v| *v <= s).clamp(1, m.max(2) - 1);
            let (p0, p1) = (pts[c][i - 1], pts[c][(i).min(m - 1)]);
            let span = (a[i.min(m - 1)] - a[i - 1]).max(1e-9);
            let u = ((s - a[i - 1]) / span).clamp(0.0, 1.0);
            (p0.0.lerp(p1.0, u), p0.1 + (p1.1 - p0.1) * u)
        };
        let label = if c == 0 {
            "trunk".to_string()
        } else if ch.parent.is_none() {
            format!("root{c}")
        } else {
            format!("branch{c}")
        };
        let base = bones.len();
        for b in 0..k {
            let (head, r) = at(b as f32 * seg);
            let (tail, _) = at((b + 1) as f32 * seg);
            let parent = if b == 0 {
                attach.map(|p| bones[p].0.clone())
            } else {
                Some(bones[base + b - 1].0.clone())
            };
            bones.push((
                format!("{label}_{b}"),
                parent,
                head,
                Some(tail),
                Some(r.max(1e-3)),
            ));
        }
        let mut wts = Vec::with_capacity(m);
        let mut prim = Vec::with_capacity(m);
        for &s in &arcs[c] {
            let u = s / seg;
            let kb = (u.floor() as usize).min(k - 1);
            let f = u - kb as f32;
            let me = base + kb;
            let prev = if kb > 0 { Some(me - 1) } else { attach };
            let w = if f < 0.25
                && let Some(p) = prev
            {
                let wp = 0.5 * (1.0 - f / 0.25);
                [(me, 1.0 - wp), (p, wp)]
            } else if f > 0.75 && kb + 1 < k {
                let wn = 0.5 * (f - 0.75) / 0.25;
                [(me, 1.0 - wn), (me + 1, wn)]
            } else {
                [(me, 1.0), (me, 0.0)]
            };
            wts.push(w);
            prim.push(me);
        }
        point.push(wts);
        primary.push(prim);
    }
    Ok((bones, point))
}

/// A bone per point of a skin op's chains: named joints keep their names,
/// other points are `c{chain}_{point}`. A mirrored skin's +x bones get `_l`
/// (the skeleton's mirror then makes the `_r` twins).
pub fn from_skin(from: &str, def: &crate::sdf::SkinDef) -> Result<(Vec<RawBone>, bool), String> {
    use crate::sdf::ChainPt;
    let mirror = match def.mirror.as_deref() {
        None => false,
        Some("x") => true,
        Some(m) => {
            return Err(format!(
                "`{from}` mirrors `{m}`: auto-rigs follow only mirror \"x\"; list the bones by hand"
            ));
        }
    };
    // Nodes: named joints once, unnamed points per chain slot.
    let mut nodes: Vec<(String, [f32; 4])> = Vec::new();
    let mut edges: Vec<(usize, usize)> = Vec::new();
    let node_of = |name: String, v: [f32; 4], nodes: &mut Vec<(String, [f32; 4])>| {
        if let Some(i) = nodes.iter().position(|n| n.0 == name) {
            i
        } else {
            nodes.push((name, v));
            nodes.len() - 1
        }
    };
    for (ci, chain) in def.chains.iter().enumerate() {
        let mut prev: Option<usize> = None;
        for (pi, p) in chain.iter().enumerate() {
            let i = match p {
                ChainPt::Joint(j) => {
                    let Some(v) = def.joints.get(j) else {
                        return Err(format!(
                            "`{from}`: chain {ci} names joint `{j}`, not in joints"
                        ));
                    };
                    node_of(j.clone(), *v, &mut nodes)
                }
                ChainPt::Point(v) => node_of(format!("c{ci}_{pi}"), *v, &mut nodes),
            };
            if let Some(a) = prev
                && a != i
            {
                edges.push((a, i));
            }
            prev = Some(i);
        }
    }
    if nodes.is_empty() {
        return Err(format!("`{from}` has no chain points"));
    }
    let deg = |i: usize| edges.iter().filter(|(a, b)| *a == i || *b == i).count();
    // Roots: the best connected named joint, then whatever is left over.
    let named: Vec<usize> = (0..nodes.len())
        .filter(|&i| def.joints.contains_key(&nodes[i].0))
        .collect();
    let first = named
        .iter()
        .copied()
        .max_by_key(|&i| (deg(i), std::cmp::Reverse(i)))
        .unwrap_or(0);
    let mut parent: Vec<Option<Option<usize>>> = vec![None; nodes.len()];
    let mut order: Vec<usize> = Vec::new();
    let mut start = Some(first);
    while let Some(r) = start {
        parent[r] = Some(None);
        let mut queue = std::collections::VecDeque::from([r]);
        while let Some(a) = queue.pop_front() {
            order.push(a);
            for &(x, y) in &edges {
                let b = if x == a {
                    y
                } else if y == a {
                    x
                } else {
                    continue;
                };
                if parent[b].is_none() {
                    parent[b] = Some(Some(a));
                    queue.push_back(b);
                }
            }
        }
        start = (0..nodes.len()).find(|&i| parent[i].is_none());
    }
    let side = |i: usize| mirror && nodes[i].1[0] > 1e-4;
    let name = |i: usize| -> String {
        let n = &nodes[i].0;
        if side(i) && super::mirror_name(n).is_none() && !n.ends_with("_r") {
            format!("{n}_l")
        } else {
            n.clone()
        }
    };
    let bones = order
        .iter()
        .map(|&i| {
            let v = nodes[i].1;
            (
                name(i),
                parent[i].flatten().map(name),
                Vec3::new(v[0], v[1], v[2]),
                None,
                Some(v[3].max(1e-3)),
            )
        })
        .collect();
    Ok((bones, mirror))
}
