//! Baked ambient occlusion for exports (`optimize: {ao: {..}}`): every
//! exported mesh gets a second uv set (glTF `TEXCOORD_1`) laid out as
//! box-unwrapped islands in one atlas per glTF (the same unwrap and packing
//! as object-domain material atlases), and each texel of that atlas holds how
//! open the hemisphere over it is, ray traced against all the export's
//! opaque geometry (so a wall darkens where it meets the floor, a floor
//! under a counter). Every material then reads it as its `occlusionTexture`
//! through `texCoord: 1`, which a tiling material shared by many meshes can
//! do because each mesh brings its own uv1.

use std::collections::HashMap;

use bevy::math::Vec3;
use rayon::prelude::*;
use serde_json::Value;

use crate::Part;
use crate::export_opt::{Bvh, glob, world_tris};
use crate::material::atlas::{GUTTER, unwrap};

/// The `ao` export option.
#[derive(Clone, Debug, PartialEq)]
pub struct Ao {
    /// Atlas width and height in texels (a power of two, 256..8192).
    pub resolution: usize,
    /// Meters: hits farther than this count as open sky.
    pub distance: f32,
    /// Cosine-weighted hemisphere rays per texel.
    pub samples: usize,
    /// Part-name patterns that get no atlas space (they read white) but
    /// still occlude others: terrain, huge ground planes.
    pub skip: Vec<String>,
}

impl Default for Ao {
    fn default() -> Self {
        Ao {
            resolution: 1024,
            distance: 1.0,
            samples: 64,
            skip: Vec::new(),
        }
    }
}

impl Ao {
    /// `true` (defaults), `false` / null (off), or `{resolution?, distance?,
    /// samples?, skip?}`.
    pub fn parse(v: &Value) -> Result<Option<Ao>, String> {
        let m = match v {
            Value::Null | Value::Bool(false) => return Ok(None),
            Value::Bool(true) => return Ok(Some(Ao::default())),
            Value::Object(m) => m,
            other => {
                return Err(format!(
                    "optimize.ao: true, false or {{resolution, distance, samples, skip}}, got {other}"
                ));
            }
        };
        let mut a = Ao::default();
        for (k, x) in m {
            match k.as_str() {
                "resolution" => {
                    a.resolution = x
                        .as_u64()
                        .filter(|n| n.is_power_of_two() && (256..=8192).contains(n))
                        .ok_or("optimize.ao.resolution: a power of two, 256..8192")?
                        as usize
                }
                "distance" => {
                    a.distance = x
                        .as_f64()
                        .filter(|d| *d > 0.0)
                        .ok_or("optimize.ao.distance: meters > 0")?
                        as f32
                }
                "samples" => {
                    a.samples =
                        x.as_u64()
                            .filter(|n| (1..=1024).contains(n))
                            .ok_or("optimize.ao.samples: 1..1024")? as usize
                }
                "skip" => {
                    a.skip = serde_json::from_value(x.clone())
                        .map_err(|e| format!("optimize.ao.skip: {e}"))?
                }
                other => {
                    return Err(format!(
                        "optimize.ao: unknown field `{other}` (resolution, distance, samples, skip)"
                    ));
                }
            }
        }
        Ok(Some(a))
    }
}

/// What a part does in the bake.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Gets atlas islands and occludes.
    Bake,
    /// Occludes others; reads a white texel.
    Occlude,
    /// Neither (decal quads, LOD copies): reads a white texel.
    Ignore,
}

/// The baked atlas and each part's uv1 (same length as its verts, after
/// [`bake`] split its vertices at island borders).
pub struct Baked {
    pub n: usize,
    /// RGBA, grey in R=G=B, 255 = open.
    pub rgba: Vec<u8>,
    pub uv1: Vec<Vec<[f32; 2]>>,
    pub report: String,
}

/// Each part's role: `skip` patterns and rigged parts occlude only.
pub fn roles(parts: &[Part], ao: &Ao, ignore: impl Fn(usize) -> bool) -> Vec<Role> {
    parts
        .iter()
        .enumerate()
        .map(|(i, p)| {
            if ignore(i) {
                Role::Ignore
            } else if p.rig.is_some() || ao.skip.iter().any(|s| glob(s, &p.name)) {
                Role::Occlude
            } else {
                Role::Bake
            }
        })
        .collect()
}

fn hash(mut x: u32) -> u32 {
    x ^= x >> 16;
    x = x.wrapping_mul(0x7feb_352d);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846c_a68b);
    x ^ (x >> 16)
}

/// Cosine-weighted directions about +Z (a Fibonacci spiral on the disk,
/// lifted to the hemisphere).
fn hemisphere(n: usize) -> Vec<Vec3> {
    let golden = std::f32::consts::PI * (3.0 - 5f32.sqrt());
    (0..n)
        .map(|i| {
            let r = ((i as f32 + 0.5) / n as f32).sqrt();
            let a = i as f32 * golden;
            Vec3::new(r * a.cos(), r * a.sin(), (1.0 - r * r).max(0.0).sqrt())
        })
        .collect()
}

/// Unwraps the `Bake` parts into one atlas (splitting their vertices at
/// island borders: the geometry and uv0 are unchanged), then ray traces
/// every covered texel against all opaque `Bake` / `Occlude` triangles.
pub fn bake(parts: &mut [Part], roles: &[Role], ao: &Ao) -> Baked {
    let start = std::time::Instant::now();
    let n = ao.resolution;
    let nf = n as f32;
    // Occluders, in world space.
    let mut occ = Vec::new();
    for (p, r) in parts.iter().zip(roles) {
        if *r != Role::Ignore && p.material.base_color[3] >= 0.999 {
            occ.extend(world_tris(p).into_iter().map(|t| (t, 0u32)));
        }
    }
    let occ_tris = occ.len();
    // The unwrap over every baked part's world-space triangles.
    let mut offset = vec![0usize; parts.len()];
    let (mut corners, mut pos, mut ok, mut owner) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut total = 0usize;
    for (pi, (p, r)) in parts.iter().zip(roles).enumerate() {
        offset[pi] = total;
        total += p.verts.len();
        if *r != Role::Bake {
            continue;
        }
        let w: Vec<Vec3> = (0..p.verts.len()).map(|i| p.world_vert(i)).collect();
        for (t, c) in p.indices.as_chunks::<3>().0.iter().enumerate() {
            let c = c.map(|k| k as usize);
            let good = c.iter().all(|&k| k < w.len());
            let q = if good {
                c.map(|k| w[k])
            } else {
                [Vec3::ZERO; 3]
            };
            corners.push(c.map(|k| k + offset[pi]));
            pos.push(q);
            ok.push(good && (q[1] - q[0]).cross(q[2] - q[0]).length_squared() > 1e-16);
            owner.push((pi, t));
        }
    }
    let u = unwrap(&corners, &pos, &ok, n);
    let islands = u
        .island
        .iter()
        .filter(|i| **i != usize::MAX)
        .max()
        .map_or(0, |m| m + 1);
    // Split each baked part's vertices per island; uv1 from the unwrap.
    let mut uv1: Vec<Vec<[f32; 2]>> = vec![Vec::new(); parts.len()];
    let mut by_part: HashMap<usize, Vec<usize>> = HashMap::new();
    for (k, (pi, _)) in owner.iter().enumerate() {
        by_part.entry(*pi).or_default().push(k);
    }
    for (pi, ks) in &by_part {
        let p = &mut parts[*pi];
        let has_n = p.normals.len() == p.verts.len();
        let has_uv = p.uvs.len() == p.verts.len();
        let (mut verts, mut normals, mut uvs, mut idx, mut u1) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let mut remap: HashMap<(usize, usize), u32> = HashMap::new();
        for &k in ks {
            if !ok[k] {
                continue;
            }
            let isl = u.island[k];
            for c in 0..3 {
                let v = corners[k][c] - offset[*pi];
                let px = u.px[k][c];
                let id = *remap.entry((v, isl)).or_insert_with(|| {
                    verts.push(p.verts[v]);
                    if has_n {
                        normals.push(p.normals[v]);
                    }
                    if has_uv {
                        uvs.push(p.uvs[v]);
                    }
                    u1.push([px[0] / nf, px[1] / nf]);
                    (verts.len() - 1) as u32
                });
                idx.push(id);
            }
        }
        p.verts = verts;
        p.normals = normals;
        p.uvs = uvs;
        p.indices = idx;
        uv1[*pi] = u1;
    }
    // Rasterize: the world position, face normal and island of each texel.
    let len = n * n;
    let mut at: Vec<Option<(Vec3, Vec3)>> = vec![None; len];
    let mut isl_of = vec![usize::MAX; len];
    for k in 0..corners.len() {
        if !ok[k] {
            continue;
        }
        let p = pos[k];
        let fnrm = (p[1] - p[0]).cross(p[2] - p[0]).normalize_or_zero();
        let q = u.px[k];
        let den =
            (q[1][1] - q[2][1]) * (q[0][0] - q[2][0]) + (q[2][0] - q[1][0]) * (q[0][1] - q[2][1]);
        if den.abs() < 1e-12 {
            continue;
        }
        let lo = [0, 1].map(|a| q.iter().map(|v| v[a]).fold(f32::MAX, f32::min));
        let hi = [0, 1].map(|a| q.iter().map(|v| v[a]).fold(f32::MIN, f32::max));
        let (x0, x1) = (
            (lo[0] - 0.5).floor().max(0.0) as usize,
            (hi[0] + 0.5).ceil().min(nf - 1.0) as usize,
        );
        let (y0, y1) = (
            (lo[1] - 0.5).floor().max(0.0) as usize,
            (hi[1] + 0.5).ceil().min(nf - 1.0) as usize,
        );
        for y in y0..=y1 {
            for x in x0..=x1 {
                let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                let a = ((q[1][1] - q[2][1]) * (px - q[2][0])
                    + (q[2][0] - q[1][0]) * (py - q[2][1]))
                    / den;
                let b = ((q[2][1] - q[0][1]) * (px - q[2][0])
                    + (q[0][0] - q[2][0]) * (py - q[2][1]))
                    / den;
                let g = 1.0 - a - b;
                if a < -1e-4 || b < -1e-4 || g < -1e-4 {
                    continue;
                }
                let i = y * n + x;
                at[i] = Some((p[0] * a + p[1] * b + p[2] * g, fnrm));
                isl_of[i] = u.island[k];
            }
        }
    }
    let covered = at.iter().filter(|a| a.is_some()).count();
    // The fraction of cosine-weighted directions that escape `distance`:
    // rasterised on the GPU when there is one, else ray traced.
    let bias = (0.25 / u.density.max(1e-6)).clamp(1e-3, 0.01);
    let idx: Vec<usize> = (0..len).filter(|&i| at[i].is_some()).collect();
    let pts: Vec<(Vec3, Vec3)> = idx.iter().filter_map(|&i| at[i]).collect();
    let tris: Vec<[Vec3; 3]> = occ.iter().map(|o| o.0).collect();
    let gpu = crate::ao_gpu::occlusion(&tris, &pts, ao.samples, ao.distance, bias);
    let how = if gpu.is_some() {
        "rasterised on the GPU"
    } else {
        "ray traced on the CPU"
    };
    let mut val = match gpu {
        Some(open) => {
            let mut val = vec![1.0f32; len];
            for (k, &i) in idx.iter().enumerate() {
                val[i] = open[k];
            }
            val
        }
        None => trace(occ, &at, ao, bias),
    };
    // A 3x3 blur inside each island takes the edge off the ray noise.
    let prev = val.clone();
    for y in 0..n {
        for x in 0..n {
            let i = y * n + x;
            if at[i].is_none() {
                continue;
            }
            let (mut s, mut w) = (0.0, 0.0);
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let (xx, yy) = (x as i32 + dx, y as i32 + dy);
                    if xx < 0 || yy < 0 || xx >= n as i32 || yy >= n as i32 {
                        continue;
                    }
                    let j = yy as usize * n + xx as usize;
                    if at[j].is_some() && isl_of[j] == isl_of[i] {
                        s += prev[j];
                        w += 1.0;
                    }
                }
            }
            val[i] = s / w;
        }
    }
    // Dilate into the gutter so filtering never reads the empty atlas.
    let mut valid: Vec<bool> = at.iter().map(Option::is_some).collect();
    for _ in 0..GUTTER as usize + 1 {
        let was = valid.clone();
        for y in 0..n {
            for x in 0..n {
                let i = y * n + x;
                if was[i] {
                    continue;
                }
                let nb = [
                    (x > 0).then(|| i - 1),
                    (x + 1 < n).then(|| i + 1),
                    (y > 0).then(|| i - n),
                    (y + 1 < n).then(|| i + n),
                ];
                if let Some(j) = nb.into_iter().flatten().find(|&j| was[j]) {
                    val[i] = val[j];
                    valid[i] = true;
                }
            }
        }
    }
    // A white spot (an empty 8x8 block) for the parts without islands.
    let block = (0..n / 8)
        .flat_map(|by| (0..n / 8).map(move |bx| (bx, by)))
        .rev()
        .find(|&(bx, by)| (0..8).all(|y| (0..8).all(|x| !valid[(by * 8 + y) * n + bx * 8 + x])))
        .unwrap_or((n / 8 - 1, n / 8 - 1));
    let white = [(block.0 * 8 + 4) as f32 / nf, (block.1 * 8 + 4) as f32 / nf];
    let mut plain = 0;
    for (pi, p) in parts.iter().enumerate() {
        if uv1[pi].len() != p.verts.len() {
            uv1[pi] = vec![white; p.verts.len()];
            plain += 1;
        }
    }
    let mut rgba = vec![255u8; len * 4];
    let mut sum = 0.0;
    for i in 0..len {
        if valid[i] {
            let g = (val[i].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            rgba[i * 4..i * 4 + 3].copy_from_slice(&[g, g, g]);
        }
        if at[i].is_some() {
            sum += val[i];
        }
    }
    let baked_tris = ok.iter().filter(|o| **o).count();
    let report = format!(
        "ambient occlusion: {n}x{n} atlas, {islands} island(s), {covered} texels baked ({:.0}% of the atlas, mean {:.2} open), \
         {:.0} texels/m ({:.1} cm/texel), {baked_tris} triangle(s) unwrapped, traced against {occ_tris} occluding triangle(s), \
         {} rays/texel within {} m ({how}), {} ms; {plain} part(s) read white (skip, rigged, LOD copies, decals)",
        100.0 * covered as f32 / len as f32,
        if covered > 0 {
            sum / covered as f32
        } else {
            1.0
        },
        u.density,
        100.0 / u.density.max(1e-6),
        ao.samples,
        ao.distance,
        start.elapsed().as_millis(),
    );
    Baked {
        n,
        rgba,
        uv1,
        report,
    }
}

/// The CPU path: the fraction of cosine-weighted rays from each texel that
/// escape `distance`.
fn trace(occ: Vec<([Vec3; 3], u32)>, at: &[Option<(Vec3, Vec3)>], ao: &Ao, bias: f32) -> Vec<f32> {
    let bvh = Bvh::new(occ);
    let dirs = hemisphere(ao.samples);
    at.par_iter()
        .enumerate()
        .map(|(i, a)| {
            let Some((p, nrm)) = a else { return 1.0 };
            if *nrm == Vec3::ZERO {
                return 1.0;
            }
            let (t1, t2) = nrm.any_orthonormal_pair();
            let rot = hash(i as u32) as f32 / u32::MAX as f32 * std::f32::consts::TAU;
            let (s, c) = rot.sin_cos();
            let o = *p + *nrm * bias;
            let mut open = 0usize;
            for d in &dirs {
                let (x, y) = (d.x * c - d.y * s, d.x * s + d.y * c);
                let dir = t1 * x + t2 * y + *nrm * d.z;
                if !bvh.walk(o, dir, ao.distance, &mut |_, _| true) {
                    open += 1;
                }
            }
            open as f32 / dirs.len() as f32
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PrimitiveKind;

    fn block(name: &str, size: [f32; 3], at: [f32; 3]) -> Part {
        let mut p = crate::primitives::primitive(PrimitiveKind::Box, size, name)
            .unwrap_or_else(|e| panic!("{e}"));
        p.translation = at;
        p
    }

    /// A floor, two walls meeting in a corner, and a box on the floor.
    fn room() -> Vec<Part> {
        vec![
            block("floor", [4.0, 0.1, 4.0], [0.0, -0.05, 0.0]),
            block("wall_x", [0.1, 2.0, 4.0], [-2.05, 1.0, 0.0]),
            block("wall_z", [4.0, 2.0, 0.1], [0.0, 1.0, -2.05]),
            block("crate", [0.6, 0.6, 0.6], [0.8, 0.3, 0.8]),
        ]
    }

    fn baked() -> (Vec<Part>, Baked) {
        let mut parts = room();
        let ao = Ao {
            resolution: 512,
            samples: 64,
            ..Ao::default()
        };
        let r = roles(&parts, &ao, |_| false);
        let b = bake(&mut parts, &r, &ao);
        (parts, b)
    }

    /// The baked openness at a world point on part `pi`'s surface.
    fn openness(parts: &[Part], b: &Baked, pi: usize, w: Vec3) -> f32 {
        let p = &parts[pi];
        for t in p.indices.as_chunks::<3>().0 {
            let c = t.map(|k| k as usize);
            let q = c.map(|k| p.world_vert(k));
            let nn = (q[1] - q[0]).cross(q[2] - q[0]);
            if nn.length() < 1e-9 || (w - q[0]).dot(nn.normalize()).abs() > 1e-3 {
                continue;
            }
            let (v0, v1, v2) = (q[1] - q[0], q[2] - q[0], w - q[0]);
            let (d00, d01, d11) = (v0.dot(v0), v0.dot(v1), v1.dot(v1));
            let (d20, d21) = (v2.dot(v0), v2.dot(v1));
            let den = d00 * d11 - d01 * d01;
            let bb = (d11 * d20 - d01 * d21) / den;
            let cc = (d00 * d21 - d01 * d20) / den;
            let aa = 1.0 - bb - cc;
            if aa < -1e-4 || bb < -1e-4 || cc < -1e-4 {
                continue;
            }
            let uv = [0, 1].map(|k| {
                b.uv1[pi][c[0]][k] * aa + b.uv1[pi][c[1]][k] * bb + b.uv1[pi][c[2]][k] * cc
            });
            let (x, y) = (
                ((uv[0] * b.n as f32) as usize).min(b.n - 1),
                ((uv[1] * b.n as f32) as usize).min(b.n - 1),
            );
            return b.rgba[(y * b.n + x) * 4] as f32 / 255.0;
        }
        panic!("{w} is not on {}", p.name);
    }

    #[test]
    fn uv1_islands_do_not_overlap() {
        let (parts, b) = baked();
        let n = b.n;
        let mut owner: Vec<Option<(usize, usize)>> = vec![None; n * n];
        for (pi, p) in parts.iter().enumerate() {
            assert_eq!(b.uv1[pi].len(), p.verts.len());
            for (ti, t) in p.indices.as_chunks::<3>().0.iter().enumerate() {
                let q = t.map(|k| {
                    let u = b.uv1[pi][k as usize];
                    assert!((0.0..=1.0).contains(&u[0]) && (0.0..=1.0).contains(&u[1]));
                    [u[0] * n as f32, u[1] * n as f32]
                });
                let den = (q[1][1] - q[2][1]) * (q[0][0] - q[2][0])
                    + (q[2][0] - q[1][0]) * (q[0][1] - q[2][1]);
                assert!(den.abs() > 1e-6, "{}: degenerate uv1 triangle", p.name);
                for y in 0..n {
                    for x in 0..n {
                        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                        let a = ((q[1][1] - q[2][1]) * (px - q[2][0])
                            + (q[2][0] - q[1][0]) * (py - q[2][1]))
                            / den;
                        let bb = ((q[2][1] - q[0][1]) * (px - q[2][0])
                            + (q[0][0] - q[2][0]) * (py - q[2][1]))
                            / den;
                        if a > 0.01 && bb > 0.01 && 1.0 - a - bb > 0.01 {
                            let i = y * n + x;
                            assert!(
                                owner[i].is_none(),
                                "texel {x},{y}: {} tri {ti} overlaps {:?}",
                                p.name,
                                owner[i]
                            );
                            owner[i] = Some((pi, ti));
                        }
                    }
                }
            }
        }
        // The floor (16 m² on top) gets real resolution: hundreds of texels across.
        let floor_texels = owner.iter().filter(|o| o.is_some_and(|o| o.0 == 0)).count();
        assert!(floor_texels > 10_000, "floor texels {floor_texels}");
    }

    #[test]
    fn corners_and_contacts_darken() {
        let (parts, b) = baked();
        let open = openness(&parts, &b, 0, Vec3::new(1.5, 0.0, -0.6));
        let by_crate = openness(&parts, &b, 0, Vec3::new(1.15, 0.0, 0.8));
        assert!(open > 0.85, "open floor {open}");
        assert!(
            by_crate < open - 0.15,
            "floor by the crate {by_crate} vs {open}"
        );
        let wall_mid = openness(&parts, &b, 1, Vec3::new(-2.0, 1.2, 0.5));
        let wall_corner = openness(&parts, &b, 1, Vec3::new(-2.0, 1.2, -1.95));
        assert!(
            wall_corner < wall_mid - 0.15,
            "wall corner {wall_corner} vs middle {wall_mid}"
        );
        let wall_foot = openness(&parts, &b, 1, Vec3::new(-2.0, 0.03, 0.5));
        assert!(
            wall_foot < wall_mid - 0.15,
            "wall foot {wall_foot} vs {wall_mid}"
        );
    }

    #[test]
    fn every_material_reads_the_atlas_through_uv1() {
        let mut model = crate::Model {
            name: "room".into(),
            ..Default::default()
        };
        for (i, mut p) in room().into_iter().enumerate() {
            p.material.base_color = [0.2 * i as f32, 0.5, 0.5, 1.0];
            model.parts.push(p.into());
        }
        let mut o = crate::export_opt::Options::game();
        o.ao = Some(Ao {
            resolution: 256,
            samples: 16,
            ..Ao::default()
        });
        let (doc, _, report) = crate::gltf::build_with(&model, None, &o, None, false)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(report.contains("ambient occlusion: 256x256"), "{report}");
        let mats = doc["materials"].as_array().cloned().unwrap_or_default();
        assert_eq!(mats.len(), 4);
        for m in &mats {
            assert_eq!(m["occlusionTexture"]["texCoord"], 1, "{m}");
        }
        let ao_index = &mats[0]["occlusionTexture"]["index"];
        assert!(
            mats.iter()
                .all(|m| &m["occlusionTexture"]["index"] == ao_index)
        );
        for mesh in doc["meshes"].as_array().into_iter().flatten() {
            let a = &mesh["primitives"][0]["attributes"];
            let pos = a["POSITION"].as_u64().unwrap_or(0) as usize;
            let uv1 = a["TEXCOORD_1"].as_u64().unwrap_or_else(|| panic!("{mesh}")) as usize;
            assert!(a.get("TEXCOORD_0").is_some());
            assert_eq!(
                doc["accessors"][pos]["count"],
                doc["accessors"][uv1]["count"]
            );
        }
    }

    #[test]
    fn skipped_copies_share_one_mesh_in_the_gltf() {
        let mut model = crate::Model {
            name: "room".into(),
            ..Default::default()
        };
        for p in room() {
            model.parts.push(p.into());
        }
        for i in 0..4 {
            let mut n = crate::primitives::primitive(crate::PrimitiveKind::Box, [0.05; 3], "n")
                .unwrap_or_else(|e| panic!("{e}"));
            n.name = format!("nut_{i}");
            n.translation = [0.2 * i as f32, 0.5, 0.0];
            model.parts.push(n.into());
        }
        let mut o = crate::export_opt::Options::game();
        o.ao = Some(Ao {
            resolution: 256,
            samples: 16,
            skip: vec!["nut_*".into()],
            ..Ao::default()
        });
        let (doc, _, report) = crate::gltf::build_with(&model, None, &o, None, false)
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(report.contains("ambient occlusion"), "{report}");
        let nodes = doc["nodes"].as_array().cloned().unwrap_or_default();
        let nut_meshes: std::collections::HashSet<u64> = nodes
            .iter()
            .filter(|n| n["name"].as_str().is_some_and(|s| s.starts_with("nut_")))
            .filter_map(|n| n["mesh"].as_u64())
            .collect();
        assert_eq!(nut_meshes.len(), 1, "four nuts, one mesh: {nodes:?}");
    }

    #[test]
    fn parse_ao() {
        assert_eq!(Ao::parse(&Value::Bool(true)), Ok(Some(Ao::default())));
        assert_eq!(Ao::parse(&Value::Null), Ok(None));
        let a = Ao::parse(&serde_json::json!({"resolution": 2048, "distance": 0.5, "samples": 32, "skip": ["ground*"]}))
            .unwrap_or_else(|e| panic!("{e}"))
            .unwrap_or_default();
        assert_eq!((a.resolution, a.distance, a.samples), (2048, 0.5, 32));
        assert!(Ao::parse(&serde_json::json!({"resolution": 1000})).is_err());
    }

    #[test]
    fn gpu_matches_the_cpu_tracer() {
        let parts = room();
        let occ: Vec<([Vec3; 3], u32)> =
            parts.iter().flat_map(world_tris).map(|t| (t, 0)).collect();
        let tris: Vec<[Vec3; 3]> = occ.iter().map(|o| o.0).collect();
        // A grid over the floor and the x wall.
        let mut pts = Vec::new();
        for i in 0..20 {
            for j in 0..20 {
                let (a, b) = (-1.9 + i as f32 * 0.19, -1.9 + j as f32 * 0.19);
                pts.push((Vec3::new(a, 0.0, b), Vec3::Y));
                pts.push((Vec3::new(-2.0, 0.05 + j as f32 * 0.09, a), Vec3::X));
            }
        }
        let ao = Ao {
            samples: 256,
            ..Ao::default()
        };
        let Some(gpu) = crate::ao_gpu::occlusion(&tris, &pts, ao.samples, ao.distance, 0.005)
        else {
            eprintln!("no GPU adapter: skipped");
            return;
        };
        let at: Vec<Option<(Vec3, Vec3)>> = pts.iter().copied().map(Some).collect();
        let cpu = trace(occ, &at, &ao, 0.005);
        let n = pts.len() as f32;
        let mean = |v: &[f32]| v.iter().sum::<f32>() / n;
        let diff = gpu
            .iter()
            .zip(&cpu)
            .map(|(a, b)| (a - b).abs())
            .sum::<f32>()
            / n;
        let worst = gpu
            .iter()
            .zip(&cpu)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        eprintln!(
            "gpu mean {:.3}, cpu mean {:.3}, mean |diff| {diff:.3}, worst {worst:.3}",
            mean(&gpu),
            mean(&cpu)
        );
        assert!((mean(&gpu) - mean(&cpu)).abs() < 0.03);
        assert!(diff < 0.05, "mean diff {diff}");
    }
}
