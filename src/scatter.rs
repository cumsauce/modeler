//! Scattering: copies of a part (or of rigid sets of parts, picked at random)
//! strewn over another part's surface. Points are sampled area-weighted on
//! the `on` part's triangles (upward-facing ones by default), rejected by
//! slope, region and spacing (dart throwing on a hash grid, so the layout is
//! Poisson-disk-like), and each copy is set down with its base on the point.
//! Deterministic per seed. No game code.

use crate::{Part, quat_of};
use bevy::math::{Quat, Vec3};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// Hard cap on copies per scatter (each copy is a part).
pub const MAX_COPIES: u32 = 10_000;
/// Copies tried when only `density` is given and `max` is not.
pub const DEFAULT_MAX: u32 = 2_000;

/// How a scatter strews its copies (lengths in meters, angles in degrees).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ScatterDef {
    /// Exact number of copies wanted (fewer when spacing/slope leave no room).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    /// Copies per square meter of eligible surface (instead of `count`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub density: Option<f32>,
    /// Upper bound with `density` (also the number of output names).
    pub max: u32,
    pub seed: u64,
    /// Uniform scale range [min, max].
    pub scale: [f32; 2],
    /// "random_y" (spin about the up axis), "random" (any orientation:
    /// tumbled rocks) or "none".
    pub rotate: String,
    /// 0 = stay upright, 1 = stand along the surface normal.
    pub align: f32,
    /// Minimum distance between copies' base points; None = derived from the
    /// copies' footprint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_spacing: Option<f32>,
    /// Steepest surface accepted (degrees between its normal and up); None
    /// = any face `faces` allows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_slope: Option<f32>,
    /// Extra depth each copy is pushed into the surface.
    pub sink: f32,
    /// Only points with x, z inside [min, max].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<[[f32; 2]; 2]>,
    /// "up" (faces whose normal points up) or "all".
    pub faces: String,
    /// Relative odds of each set (default equal).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub weights: Vec<f32>,
}

impl Default for ScatterDef {
    fn default() -> Self {
        Self {
            count: None,
            density: None,
            max: DEFAULT_MAX,
            seed: 1,
            scale: [1.0, 1.0],
            rotate: "random_y".into(),
            align: 0.0,
            min_spacing: None,
            max_slope: None,
            sink: 0.0,
            region: None,
            faces: "up".into(),
            weights: Vec::new(),
        }
    }
}

impl ScatterDef {
    /// Copies tried (and output names reserved).
    pub fn slots(&self) -> u32 {
        self.count.unwrap_or(self.max).min(MAX_COPIES)
    }
}

/// The suffix each member of a set gets after `name_i`: the first member is
/// `name_i`; `oak_leaves` beside `oak` is `name_i_leaves`; members of one
/// prefab instance (`tree1.trunk`, `tree1.crown`) become `name_i.trunk`,
/// `name_i.crown`; anything else `name_i.member`.
pub fn member_suffixes(set: &[String]) -> Vec<String> {
    if set.len() == 1 {
        return vec![String::new()];
    }
    let prefix = set[0].split_once('.').map(|(p, _)| format!("{p}."));
    if let Some(p) = &prefix
        && set
            .iter()
            .all(|m| m.starts_with(p.as_str()) && m.len() > p.len())
    {
        return set.iter().map(|m| format!(".{}", &m[p.len()..])).collect();
    }
    let first = &set[0];
    set.iter()
        .enumerate()
        .map(|(i, m)| {
            if i == 0 {
                String::new()
            } else if let Some(rest) = m.strip_prefix(first.as_str())
                && (rest.starts_with('_') || rest.starts_with('.'))
            {
                rest.to_string()
            } else {
                format!(".{m}")
            }
        })
        .collect()
}

/// Every name a scatter may write: `name_i` plus each set member's suffix,
/// for every slot.
pub fn output_names(name: &str, sets: &[Vec<String>], def: &ScatterDef) -> Vec<String> {
    let mut suffixes: Vec<String> = Vec::new();
    for s in sets {
        for x in member_suffixes(s) {
            if !suffixes.contains(&x) {
                suffixes.push(x);
            }
        }
    }
    if suffixes.is_empty() {
        suffixes.push(String::new());
    }
    let n = def.slots();
    let mut out = Vec::with_capacity(n as usize * suffixes.len());
    for i in 0..n {
        for s in &suffixes {
            out.push(format!("{name}_{i}{s}"));
        }
    }
    out
}

/// splitmix64: small, fast, deterministic per seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in [0, 1).
    fn f(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// Where a set stands: the centroid of its lowest slice (the trunk of a
/// tree, not the middle of its crown) at its lowest height, and the slice's
/// vertices (sunk below the surface's tangent plane when placed).
struct Base {
    point: Vec3,
    slice: Vec<usize>,
    height: f32,
    /// Horizontal extent (max of x and z widths), for the default spacing.
    width: f32,
}

/// The base of `verts` turned by `turn` about `center`, in the turned frame
/// (returned point is mapped back to world coordinates).
fn base_of(verts: &[Vec3], turn: Quat, center: Vec3) -> Base {
    let w: Vec<Vec3> = verts.iter().map(|v| turn * (*v - center)).collect();
    let mut min = Vec3::splat(f32::MAX);
    let mut max = Vec3::splat(f32::MIN);
    for v in &w {
        min = min.min(*v);
        max = max.max(*v);
    }
    let height = (max.y - min.y).max(1e-6);
    let cut = min.y + 0.05 * height;
    let (mut sum, mut n) = (Vec3::ZERO, 0.0f32);
    for v in w.iter().filter(|v| v.y <= cut) {
        sum += *v;
        n += 1.0;
    }
    let c = if n > 0.0 { sum / n } else { (min + max) * 0.5 };
    let foot = Vec3::new(c.x, min.y, c.z);
    Base {
        point: center + turn.inverse() * foot,
        slice: (0..w.len()).filter(|&i| w[i].y <= cut).collect(),
        height,
        width: (max.x - min.x).max(max.z - min.z),
    }
}

/// One candidate surface triangle (world space).
struct Tri {
    a: Vec3,
    b: Vec3,
    c: Vec3,
    n: Vec3,
}

/// Places the copies. `sets[k]` holds the parts of set k; returns
/// `(name, part)` pairs to add, in slot order.
pub fn build(
    name: &str,
    sets: &[Vec<Arc<Part>>],
    on: &Part,
    def: &ScatterDef,
) -> Result<Vec<(String, Part)>, String> {
    if sets.is_empty() || sets.iter().any(Vec::is_empty) {
        return Err("input: name at least one part to scatter".into());
    }
    // Candidate triangles, filtered by facing, slope and region.
    let cos_slope = def
        .max_slope
        .map_or(-2.0, |d| d.clamp(0.0, 180.0).to_radians().cos());
    let up_only = def.faces != "all";
    let mut tris = Vec::new();
    let mut cdf = Vec::new();
    let mut area = 0.0f32;
    for t in on.indices.as_chunks::<3>().0 {
        let [a, b, c] = t.map(|i| on.world_vert(i as usize));
        let cr = (b - a).cross(c - a);
        let len = cr.length();
        if len < 1e-12 {
            continue;
        }
        let n = cr / len;
        if (up_only && n.y <= 1e-4) || n.y < cos_slope - 1e-6 {
            continue;
        }
        if let Some([lo, hi]) = def.region {
            let (mnx, mxx) = (a.x.min(b.x).min(c.x), a.x.max(b.x).max(c.x));
            let (mnz, mxz) = (a.z.min(b.z).min(c.z), a.z.max(b.z).max(c.z));
            if mxx < lo[0] || mnx > hi[0] || mxz < lo[1] || mnz > hi[1] {
                continue;
            }
        }
        area += len * 0.5;
        cdf.push(area);
        tris.push(Tri { a, b, c, n });
    }
    if tris.is_empty() {
        return Err(format!(
            "`{}` has no {} surface{}{} to scatter on{}",
            on.name,
            if up_only { "upward-facing" } else { "" },
            match def.max_slope {
                Some(m) => format!(" under max_slope {m}°"),
                None => String::new(),
            },
            if def.region.is_some() {
                " inside region"
            } else {
                ""
            },
            if up_only {
                " (faces: \"all\" uses every face)"
            } else {
                ""
            }
        ));
    }
    let target = match (def.count, def.density) {
        (Some(c), _) => c,
        (None, Some(d)) => ((d.max(0.0) * area).round() as u32).min(def.max),
        (None, None) => return Err("give count or density".into()),
    }
    .min(MAX_COPIES);

    // Each set's world vertices and its upright base (cached; a "random"
    // rotation recomputes the base per copy).
    let mut set_verts: Vec<Vec<Vec3>> = Vec::with_capacity(sets.len());
    for s in sets {
        let mut v = Vec::new();
        for p in s {
            v.extend((0..p.verts.len()).map(|i| p.world_vert(i)));
        }
        if v.is_empty() {
            return Err(format!("input `{}` has no vertices", s[0].name));
        }
        set_verts.push(v);
    }
    let centers: Vec<Vec3> = set_verts
        .iter()
        .map(|v| {
            let (mn, mx) = v.iter().fold(
                (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)),
                |(a, b), p| (a.min(*p), b.max(*p)),
            );
            (mn + mx) * 0.5
        })
        .collect();
    let upright: Vec<Base> = set_verts
        .iter()
        .zip(&centers)
        .map(|(v, c)| base_of(v, Quat::IDENTITY, *c))
        .collect();
    let [s_lo, s_hi] = {
        let [a, b] = def.scale;
        [a.min(b).max(1e-4), a.max(b).max(1e-4)]
    };
    let spacing = def
        .min_spacing
        .unwrap_or_else(|| 0.8 * s_hi * upright.iter().map(|b| b.width).fold(0.0f32, f32::max));
    let weights: Vec<f32> = (0..sets.len())
        .map(|k| def.weights.get(k).copied().unwrap_or(1.0).max(0.0))
        .collect();
    let wsum: f32 = weights.iter().sum();
    if wsum <= 0.0 {
        return Err("weights: at least one set needs a weight > 0".into());
    }

    let mut rng = Rng(def.seed ^ 0x5CA7_7E25_0000_0001);
    let cell = spacing.max(1e-4);
    let key = |p: Vec3| {
        (
            (p.x / cell).floor() as i32,
            (p.y / cell).floor() as i32,
            (p.z / cell).floor() as i32,
        )
    };
    let mut grid: HashMap<(i32, i32, i32), Vec<Vec3>> = HashMap::new();
    let mut points: Vec<(Vec3, Vec3)> = Vec::new();
    let attempts = (target as usize).saturating_mul(30).max(64);
    for _ in 0..attempts {
        if points.len() >= target as usize {
            break;
        }
        let r = rng.f() * area;
        let ti = cdf.partition_point(|&c| c < r).min(tris.len() - 1);
        let t = &tris[ti];
        let (mut u, mut v) = (rng.f(), rng.f());
        if u + v > 1.0 {
            u = 1.0 - u;
            v = 1.0 - v;
        }
        let p = t.a + (t.b - t.a) * u + (t.c - t.a) * v;
        if let Some([lo, hi]) = def.region
            && (p.x < lo[0] || p.x > hi[0] || p.z < lo[1] || p.z > hi[1])
        {
            continue;
        }
        if spacing > 0.0 {
            let (kx, ky, kz) = key(p);
            let mut close = false;
            'search: for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        if let Some(list) = grid.get(&(kx + dx, ky + dy, kz + dz))
                            && list
                                .iter()
                                .any(|q| q.distance_squared(p) < spacing * spacing)
                        {
                            close = true;
                            break 'search;
                        }
                    }
                }
            }
            if close {
                continue;
            }
            grid.entry(key(p)).or_default().push(p);
        }
        points.push((p, t.n));
    }

    let mut out = Vec::new();
    let tau = std::f32::consts::TAU;
    for (i, (p, n)) in points.into_iter().enumerate() {
        // Draws in a fixed order per copy so one copy's choices never shift
        // another's.
        let pick = rng.f() * wsum;
        let spin = rng.f() * tau;
        let s = s_lo + (s_hi - s_lo) * rng.f();
        let (r1, r2, r3) = (rng.f(), rng.f(), rng.f());
        let mut acc = 0.0;
        let k = weights
            .iter()
            .position(|w| {
                acc += w;
                pick < acc
            })
            .unwrap_or(sets.len() - 1);
        let up = Vec3::Y
            .lerp(n, def.align.clamp(0.0, 1.0))
            .normalize_or(Vec3::Y);
        let align = Quat::from_rotation_arc(Vec3::Y, up);
        let (turn, yaw) = match def.rotate.as_str() {
            "none" => (Quat::IDENTITY, Quat::IDENTITY),
            "random" => {
                // Uniform random orientation (Shoemake).
                let (a, b) = ((1.0 - r1).sqrt(), r1.sqrt());
                let (t1, t2) = (tau * r2, tau * r3);
                let q = Quat::from_xyzw(a * t1.sin(), a * t1.cos(), b * t2.sin(), b * t2.cos());
                (q.normalize(), Quat::IDENTITY)
            }
            _ => (Quat::IDENTITY, Quat::from_rotation_y(spin)),
        };
        let fresh;
        let base = if turn == Quat::IDENTITY {
            &upright[k]
        } else {
            fresh = base_of(&set_verts[k], turn, centers[k]);
            &fresh
        };
        let rot = align * yaw * turn;
        // Sink until the whole base slice is at or below the surface's
        // tangent plane: a trunk upright on a slope has no downhill gap, a
        // tumbled box rests on its lowest corners. Plus a hair for contact.
        let lift = base
            .slice
            .iter()
            .map(|&i| n.dot(rot * (s * (set_verts[k][i] - base.point))))
            .fold(0.0f32, f32::max)
            // A big rock on a cliff is not buried past 15% of its height
            // (its foot still meets the surface).
            .min(0.15 * s * base.height);
        let sink = def.sink + lift + (0.002 * s * base.height).min(0.005);
        let at = p - up * sink;
        let suffixes = member_suffixes(&sets[k].iter().map(|p| p.name.clone()).collect::<Vec<_>>());
        for (src, suffix) in sets[k].iter().zip(suffixes) {
            let mut part = (**src).clone();
            part.name = format!("{name}_{i}{suffix}");
            part.translation =
                (at + rot * (s * (Vec3::from(src.translation) - base.point))).to_array();
            part.rotation = (rot * quat_of(src.rotation)).normalize().to_array();
            part.scale = (Vec3::from(src.scale) * s).to_array();
            part.visible = true;
            part.occluders = None;
            out.push((part.name.clone(), part));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use crate::commands::{Session, VerifySpec};
    use bevy::math::Vec3;
    use serde_json::{Value, json};

    fn ok<T, E: std::fmt::Display>(r: Result<T, E>) -> T {
        r.unwrap_or_else(|e| panic!("{e}"))
    }

    /// A 10 x 10 m slab whose top is at `y`, tilted `tilt` degrees about x,
    /// and a 0.15 m rock.
    fn scene(tilt: f32) -> Session {
        let mut s = Session::default();
        ok(s.apply_values(
            Some([("h".to_string(), 0.2)].into_iter().collect()),
            vec![
                json!({"type": "primitive", "name": "ground", "kind": "box",
                       "size": [10, "$h", 10], "translation": [0, "=$h / 2", 0],
                       "rotation": {"x": tilt}}),
                json!({"type": "primitive", "name": "rock", "kind": "sphere", "size": 0.15,
                       "translation": [20, 3, 0]}),
                json!({"type": "material", "name": "*", "color": "#888", "flat": true}),
            ],
        ));
        s
    }

    fn scatter(s: &mut Session, extra: Value) -> String {
        let mut item = json!({"type": "scatter", "name": "rocks", "input": "rock", "on": "ground",
                              "count": 30, "seed": 4});
        if let (Some(o), Some(e)) = (item.as_object_mut(), extra.as_object()) {
            for (k, v) in e {
                o.insert(k.clone(), v.clone());
            }
        }
        ok(s.apply_values(
            None,
            vec![item, json!({"type": "hide_part", "part": "rock"})],
        ))
    }

    fn copies(s: &Session) -> Vec<Vec3> {
        let mut out = Vec::new();
        while let Some(p) = s.model.part(&format!("rocks_{}", out.len())) {
            out.push(Vec3::from(p.translation));
        }
        out
    }

    #[test]
    fn places_the_count_and_is_deterministic() {
        let mut s = scene(0.0);
        let text = scatter(&mut s, json!({}));
        assert!(text.contains("placed 30"), "{text}");
        let a = copies(&s);
        assert_eq!(a.len(), 30);
        let mut t = scene(0.0);
        scatter(&mut t, json!({}));
        assert_eq!(a, copies(&t), "same seed, same layout");
        scatter(&mut t, json!({"seed": 5}));
        assert_ne!(a, copies(&t), "another seed moves them");
        // Inside the slab's top, on its surface.
        for p in &a {
            assert!(p.x.abs() <= 5.0 && p.z.abs() <= 5.0, "{p}");
        }
    }

    #[test]
    fn every_copy_touches_the_surface_and_verify_grounded_passes() {
        let mut s = scene(12.0);
        scatter(&mut s, json!({"scale": [0.5, 1.5], "rotate": "random"}));
        let ground = ok(s.model.part("ground").ok_or("no ground"));
        let g = crate::contact::Body::new(ground);
        for i in 0..30 {
            let p = ok(s.model.part(&format!("rocks_{i}")).ok_or("missing copy"));
            assert!(p.visible, "copies are visible though the source is hidden");
            let b = crate::contact::Body::new(p);
            assert!(
                crate::contact::contact(&b, &g).is_some(),
                "rocks_{i} floats"
            );
        }
        let spec: VerifySpec = ok(serde_json::from_value(json!({"grounded": true})));
        let (pass, text) = s.verify(&spec);
        assert!(pass, "{text}");
    }

    #[test]
    fn spacing_and_slope_limits_hold() {
        let mut s = scene(0.0);
        let text = scatter(
            &mut s,
            json!({"count": 400, "min_spacing": 1.2, "rotate": "none"}),
        );
        let pts = copies(&s);
        assert!(pts.len() < 400 && pts.len() > 20, "{}: {text}", pts.len());
        assert!(text.contains("of 400"), "{text}");
        for (i, a) in pts.iter().enumerate() {
            for b in &pts[i + 1..] {
                assert!(a.distance(*b) >= 1.2 - 1e-3, "{a} {b}");
            }
        }
        // A 40° slope refuses max_slope 30 (only its 40° top would qualify;
        // the sides are steeper still) but takes 45.
        let mut t = scene(40.0);
        let err = t
            .apply_values(
                None,
                vec![json!({"type": "scatter", "name": "rocks", "input": "rock",
                            "on": "ground", "count": 5, "max_slope": 30})],
            )
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(err.contains("max_slope"), "{err}");
        scatter(&mut t, json!({"max_slope": 45}));
        assert_eq!(copies(&t).len(), 30);
    }

    #[test]
    fn region_limits_where_they_land() {
        let mut s = scene(0.0);
        scatter(
            &mut s,
            json!({"min_spacing": 0, "region": {"min": [1, -1], "max": [3, 2]}}),
        );
        for p in copies(&s) {
            assert!(p.x >= 0.8 && p.x <= 3.2 && p.z >= -1.2 && p.z <= 2.2, "{p}");
        }
    }

    #[test]
    fn follows_a_changed_on_part() {
        let mut s = scene(0.0);
        scatter(&mut s, json!({"rotate": "none"}));
        let low = copies(&s)[0].y;
        ok(s.apply_values(
            Some([("h".to_string(), 1.2)].into_iter().collect()),
            Vec::new(),
        ));
        let high = copies(&s)[0].y;
        assert!((high - low - 1.0).abs() < 1e-3, "{low} -> {high}");
    }

    #[test]
    fn a_glob_material_skips_slots_that_did_not_fit() {
        // 500 rocks can't fit 10 x 10 m at the default spacing: the pattern
        // material must colour the ones placed and skip the rest, and names
        // of moves and places never count as parts.
        let mut s = scene(0.0);
        ok(s.apply_values(
            None,
            vec![
                json!({"type": "scatter", "name": "rocks", "input": "rock", "on": "ground",
                       "count": 500, "seed": 1, "min_spacing": 2}),
                json!({"type": "move", "name": "rocks_nudge", "parts": ["rocks_0"],
                       "translation": [0, 0, 0]}),
                json!({"type": "material", "name": "rocks_*", "color": "#a33", "flat": true}),
                json!({"type": "hide_part", "part": "rock"}),
            ],
        ));
        let placed = copies(&s).len();
        assert!(placed > 0 && placed < 500, "{placed}");
        for p in s
            .model
            .parts
            .iter()
            .filter(|p| p.name.starts_with("rocks_"))
        {
            assert!(p.material.base_color[0] > 0.5, "{} not coloured", p.name);
        }
    }

    #[test]
    fn density_sets_and_lsystem_leaves_come_along() {
        let mut s = scene(0.0);
        ok(s.apply_values(
            None,
            vec![
                json!({"type": "lsystem", "name": "shrub", "preset": "bush", "seed": 2,
                       "translation": [30, 0, 0]}),
                json!({"type": "primitive", "name": "pebble", "kind": "box", "size": 0.1,
                       "translation": [25, 0.05, 0]}),
                json!({"type": "primitive", "name": "pebble_top", "kind": "box", "size": 0.05,
                       "translation": [25, 0.125, 0]}),
                json!({"type": "scatter", "name": "mix", "on": "ground", "density": 0.1,
                       "input": ["shrub", ["pebble", "pebble_top"]], "seed": 3}),
            ],
        ));
        let n = (0..)
            .take_while(|i| s.model.part(&format!("mix_{i}")).is_some())
            .count();
        assert!((5..=10).contains(&n), "density 0.1 on 100 m²: {n}");
        let mut leaves = 0;
        let mut tops = 0;
        for i in 0..n {
            leaves += usize::from(s.model.part(&format!("mix_{i}_leaves")).is_some());
            tops += usize::from(s.model.part(&format!("mix_{i}_top")).is_some());
        }
        assert!(
            leaves > 0 && tops > 0 && leaves + tops == n,
            "{leaves} {tops} {n}"
        );
        // The pebble's top rides on its pebble.
        for i in 0..n {
            if let (Some(a), Some(b)) = (
                s.model.part(&format!("mix_{i}")),
                s.model.part(&format!("mix_{i}_top")),
            ) {
                let d = Vec3::from(b.translation) - Vec3::from(a.translation);
                assert!((d.length() - 0.075).abs() < 1e-3, "{d}");
            }
        }
    }

    #[test]
    fn suffixes_name_set_members() {
        let v = |x: &[&str]| x.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(super::member_suffixes(&v(&["oak"])), v(&[""]));
        assert_eq!(
            super::member_suffixes(&v(&["oak", "oak_leaves"])),
            v(&["", "_leaves"])
        );
        assert_eq!(
            super::member_suffixes(&v(&["t1.trunk", "t1.crown"])),
            v(&[".trunk", ".crown"])
        );
        assert_eq!(super::member_suffixes(&v(&["a", "b"])), v(&["", ".b"]));
    }
}

#[cfg(test)]
mod solid_copies {
    use crate::commands::Session;
    use serde_json::json;
    #[test]
    /// Rotated, scaled copies far from the origin still weld into closed
    /// solids (contact, collisions and booleans need them).
    fn rotated_copies_stay_closed_solids() {
        let mut s = Session::default();
        s.apply_values(None, vec![
            json!({"type":"primitive","name":"slab","kind":"box","size":[30,1,30],"translation":[0,-0.2,0],"rotation":{"x":6}}),
            json!({"type":"primitive","name":"rock_a_raw","kind":"sphere","size":0.35,"translation":[40,0,0]}),
            json!({"type":"transform","name":"rock_a","input":"rock_a_raw","scale":[1.3,0.7,1]}),
            json!({"type":"primitive","name":"rock_b","kind":"box","size":0.4,"translation":[42,0,0]}),
            json!({"type":"lsystem","name":"fir","preset":"pine","seed":5,"translation":[55,0,0]}),
            json!({"type":"scatter","name":"rocks","input":["rock_a","rock_b"],"on":"slab","count":40,"seed":2,"scale":[0.5,1.6],"rotate":"random","align":0.5}),
            json!({"type":"scatter","name":"trees","input":"fir","on":"slab","count":10,"seed":7,"scale":[0.7,1.2]}),
        ]).unwrap_or_else(|e| panic!("{e}"));
        let mut bad = Vec::new();
        for p in &s.model.parts {
            if let Err(e) = crate::csg::part_to_manifold(p) {
                bad.push(format!(
                    "{} rot={:?} scale={:?}: {}",
                    p.name,
                    p.rotation,
                    p.scale,
                    e.to_string().chars().take(90).collect::<String>()
                ));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }
}

#[cfg(test)]
mod hilly_ground {
    use crate::commands::Session;
    use serde_json::json;
    #[test]
    fn tumbled_rocks_on_hills_all_touch_the_ground() {
        let mut s = Session::default();
        s.apply_values(None, vec![
            json!({"type":"primitive","name":"slab","kind":"box","size":[30,1,30],"translation":[0,-0.2,0],"rotation":{"x":6}}),
            json!({"type":"primitive","name":"hill_a_s","kind":"sphere","size":1}),
            json!({"type":"transform","name":"hill_a","input":"hill_a_s","translation":[-5,0,-3],"scale":[7,2.2,6]}),
            json!({"type":"transform","name":"hill_b","input":"hill_a_s","translation":[6,0,4],"scale":[5,1.5,5]}),
            json!({"type":"boolean","name":"g1","op":"union","a":"slab","b":"hill_a"}),
            json!({"type":"boolean","name":"ground","op":"union","a":"g1","b":"hill_b"}),
            json!({"type":"primitive","name":"rock_a_raw","kind":"sphere","size":0.35,"translation":[40,0,0]}),
            json!({"type":"transform","name":"rock_a","input":"rock_a_raw","scale":[1.3,0.7,1]}),
            json!({"type":"primitive","name":"rock_b","kind":"box","size":0.4,"translation":[42,0,0]}),
            json!({"type":"scatter","name":"rocks","input":["rock_a","rock_b"],"on":"ground","count":80,"seed":2,"scale":[0.5,1.6],"rotate":"random","align":0.5}),
        ]).unwrap_or_else(|e| panic!("{e}"));
        let g = crate::contact::Body::new(s.model.part("ground").unwrap_or_else(|| panic!()));
        let mut bad = Vec::new();
        for i in 0..80 {
            let p = s
                .model
                .part(&format!("rocks_{i}"))
                .unwrap_or_else(|| panic!());
            let b = crate::contact::Body::new(p);
            if crate::contact::contact(&b, &g).is_none() {
                let solid = crate::csg::part_to_manifold(p).is_ok();
                let (lo, hi) = p.bounds();
                bad.push(format!(
                    "rocks_{i} solid={solid} t={:?} lo={lo} hi={hi} scale={:?} verts={}",
                    p.translation,
                    p.scale,
                    p.verts.len()
                ));
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }
}
