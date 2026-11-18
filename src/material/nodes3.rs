//! Artwork nodes: `svg` (an SVG drawing as an exact signed distance field
//! plus its own colours) and `project` (planar coordinates on one side of
//! a part, so 2D artwork lands on a named face in the object domain).

use std::collections::HashSet;
use std::sync::Mutex;

use serde::Deserialize;

use super::node::{Buf, Cx, Eval, In, Outs, pf, pv2, pv3, srgb_to_linear};

/// Port names made at run time (one per SVG element id) live as long as
/// the process; each distinct name is leaked once.
fn intern(s: &str) -> &'static str {
    static NAMES: Mutex<Option<HashSet<&'static str>>> = Mutex::new(None);
    let Ok(mut g) = NAMES.lock() else {
        return "";
    };
    let set = g.get_or_insert_with(HashSet::new);
    if let Some(n) = set.get(s) {
        return n;
    }
    let n: &'static str = Box::leak(s.to_string().into_boxed_str());
    set.insert(n);
    n
}

/// A number (width; height from the drawing's aspect) or [w, h].
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum Size2 {
    One(f32),
    Two([f32; 2]),
}

/// An SVG drawing as a signed distance field (meters, negative inside),
/// like the other 2D SDF nodes, so `mask`, `sdf_op`, `onion`, `displace`
/// and heights all work on it. `svg` is the markup (a whole `<svg>` or a
/// fragment such as `<path d="..."/>`), or `path` a .svg file (absolute,
/// or relative to the project, else the server's working directory).
/// The document box (its viewBox; a fragment's content bounds) is fitted
/// into `size` meters centred on `center` (default [0.5, 0.5]), turned by
/// `angle` degrees counter-clockwise; `fit` contain (default, aspect kept)
/// or stretch. SVG's y-down matches the image rows, so the drawing reads
/// upright. Ports: default = union of every filled shape (strokes become
/// outlines, text becomes glyphs); one port per element `id`
/// (`logo.ring`); `colors` = the drawing in its own fill colours (linear,
/// anti-aliased, painter's order); `alpha` = its coverage.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Svg {
    #[serde(default)]
    #[serde(alias = "input")]
    pub coords: Option<In>,
    /// SVG markup.
    #[serde(default)]
    pub svg: Option<String>,
    /// Or a .svg file.
    #[serde(default)]
    pub path: Option<String>,
    /// Meters: [w, h], or the width alone.
    pub size: Size2,
    #[serde(default)]
    pub center: Option<[f32; 2]>,
    /// Degrees, counter-clockwise as seen on the image.
    #[serde(default)]
    pub angle: f32,
    /// contain (default) or stretch.
    #[serde(default)]
    pub fit: Option<String>,
}

/// Resolves an artwork file: absolute, else in the project, else as given
/// (the server's working directory).
pub fn art_path(p: &str) -> std::path::PathBuf {
    let path = std::path::PathBuf::from(p);
    if path.is_absolute() {
        return path;
    }
    if let Some(root) = crate::catalog::project_root() {
        let j = root.join(&path);
        if j.exists() {
            return j;
        }
    }
    path
}

impl Svg {
    fn source(&self) -> Result<String, String> {
        match (&self.svg, &self.path) {
            (Some(s), None) => Ok(s.clone()),
            (None, Some(p)) => {
                std::fs::read_to_string(art_path(p)).map_err(|e| format!("path `{p}`: {e}"))
            }
            _ => Err("give svg (markup) or path (a .svg file), one of them".into()),
        }
    }
}

impl Eval for Svg {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let art = crate::svg::parse(&self.source()?).map_err(|e| format!("svg: {e}"))?;
        let [dw, dh] = art.size;
        let (sx, sy) = match (&self.size, self.fit.as_deref().unwrap_or("contain")) {
            (Size2::One(w), _) => (*w / dw, *w / dw),
            (Size2::Two([w, h]), "contain") => {
                let k = (w / dw).min(h / dh);
                (k, k)
            }
            (Size2::Two([w, h]), "stretch") => (w / dw, h / dh),
            (_, o) => return Err(format!("fit: `{o}`: use contain or stretch")),
        };
        if !(sx > 0.0 && sy > 0.0 && sx.is_finite() && sy.is_finite()) {
            return Err("size: meters, above 0".into());
        }
        let c0 = self.center.unwrap_or([0.5, 0.5]);
        let mid = [art.origin[0] + dw * 0.5, art.origin[1] + dh * 0.5];
        let (sn, cs) = self.angle.to_radians().sin_cos();
        let k = sx.min(sy);
        let to_art = move |p: [f32; 2]| -> [f32; 2] {
            // Image coords (y down) -> y up, undo the turn, back down.
            let (x, y) = (p[0] - c0[0], -(p[1] - c0[1]));
            let (x, y) = (x * cs + y * sn, -x * sn + y * cs);
            [mid[0] + x / sx, mid[1] - y / sy]
        };
        let c = cx.coords2(&self.coords)?;
        let n = cx.n();
        let pts: Vec<[f32; 2]> = (0..n).map(|i| to_art(c.v2(i))).collect();
        let mut outs: Outs = vec![("", pf(n, |i| art.sdf(pts[i], None) * k))];
        // Colours: each shape anti-aliased over one texel, composited in order.
        let aa = (cx.s.texel / k).max(1e-9);
        let paint: Vec<([f32; 3], f32)> = {
            use rayon::prelude::*;
            pts.par_iter()
                .map(|&p| {
                    let (mut col, mut a) = ([0.0f32; 3], 0.0f32);
                    for s in &art.shapes {
                        if crate::svg::box_dist(p, s.lo, s.hi) > aa {
                            continue;
                        }
                        let cov = (0.5 - s.grid.sdf(p) / aa).clamp(0.0, 1.0) * s.color[3];
                        if cov <= 0.0 {
                            continue;
                        }
                        let lin = [s.color[0], s.color[1], s.color[2]].map(srgb_to_linear);
                        for j in 0..3 {
                            col[j] = col[j] * (1.0 - cov) + lin[j] * cov;
                        }
                        a = a + cov * (1.0 - a);
                    }
                    // Straight (not premultiplied) colour.
                    (
                        if a > 1e-6 {
                            col.map(|v| (v / a).min(1.0))
                        } else {
                            col
                        },
                        a,
                    )
                })
                .collect()
        };
        outs.push(("colors", pv3(n, |i| paint[i].0)));
        outs.push(("alpha", pf(n, |i| paint[i].1)));
        for id in art.ids.iter().take(64) {
            if id.is_empty() || id == "colors" || id == "alpha" || id.contains('.') {
                continue;
            }
            let name = intern(id);
            outs.push((name, pf(n, |i| art.sdf(pts[i], Some(id)) * k)));
        }
        Ok(outs)
    }
}

/// Planar coordinates on one side of a part (object space, meters), for
/// putting 2D artwork (`svg`, `text`, shapes) on a named face without
/// knowing the atlas. `face` front / back / left / right / top / bottom
/// (the part's own axes; front = +z), or `normal` [x, y, z] with an
/// optional `up`. `at` is where the artwork's origin sits, in the face's
/// two cross axes like decals and `place` (front/back [x, y], left/right
/// [z, y], top/bottom [x, z]; with `normal`, [right, up] or a 3D point).
/// Output (default port): vec2 meters with `at` at [0, 0], +x to the
/// right as seen looking at the face, +y down (image rows), so give the
/// artwork `"center": [0, 0]`. `facing`: 1 on surfaces facing that way
/// (cosine smoothstepped `lo` 0.6 .. `hi` 0.85), 0 elsewhere; `depth`
/// (meters) also zeroes everything further than that behind the part's
/// frontmost point along the face direction. Multiply the artwork's mask
/// by `facing` so it lands on that side only.
#[derive(Clone, Debug, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Project {
    #[serde(default)]
    pub face: Option<String>,
    #[serde(default)]
    pub normal: Option<[f32; 3]>,
    #[serde(default)]
    pub up: Option<[f32; 3]>,
    #[serde(default)]
    pub at: Option<Vec<f32>>,
    #[serde(default)]
    pub depth: Option<f32>,
    #[serde(default)]
    pub lo: Option<f32>,
    #[serde(default)]
    pub hi: Option<f32>,
}

fn norm(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-12);
    v.map(|x| x / l)
}
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

impl Project {
    /// (direction, right, up, at in [right, up]).
    fn frame(&self) -> Result<([f32; 3], [f32; 3], [f32; 3], [f32; 2]), String> {
        let at = self.at.clone().unwrap_or_default();
        let a2 = |k: usize| at.get(k).copied().unwrap_or(0.0);
        if at.len() > 3 {
            return Err("at: two numbers (or a 3D point with normal)".into());
        }
        let (d, r, u, p3) = match (self.face.as_deref(), self.normal) {
            (Some(_), Some(_)) => return Err("give face or normal, not both".into()),
            (Some(f), None) => {
                if at.len() == 3 {
                    return Err("at: the face's two cross axes, e.g. front [x, y]".into());
                }
                let (a, b) = (a2(0), a2(1));
                match f {
                    "front" => ([0., 0., 1.], [1., 0., 0.], [0., 1., 0.], [a, b, 0.]),
                    "back" => ([0., 0., -1.], [-1., 0., 0.], [0., 1., 0.], [a, b, 0.]),
                    "right" => ([1., 0., 0.], [0., 0., -1.], [0., 1., 0.], [0., b, a]),
                    "left" => ([-1., 0., 0.], [0., 0., 1.], [0., 1., 0.], [0., b, a]),
                    "top" => ([0., 1., 0.], [1., 0., 0.], [0., 0., -1.], [a, 0., b]),
                    "bottom" => ([0., -1., 0.], [1., 0., 0.], [0., 0., 1.], [a, 0., b]),
                    o => {
                        return Err(format!(
                            "face: `{o}`: use front, back, left, right, top or bottom"
                        ));
                    }
                }
            }
            (None, Some(nrm)) => {
                let d = norm(nrm);
                let up0 = self.up.unwrap_or(if d[1].abs() > 0.9 {
                    [0.0, 0.0, -d[1].signum()]
                } else {
                    [0.0, 1.0, 0.0]
                });
                let r = norm(cross(up0, d));
                let u = cross(d, r);
                if at.len() == 3 {
                    let p = [at[0], at[1], at[2]];
                    return Ok((d, r, u, [dot(p, r), dot(p, u)]));
                }
                return Ok((d, r, u, [a2(0), a2(1)]));
            }
            (None, None) => return Err("give face (front, back, ...) or normal".into()),
        };
        if self.up.is_some() {
            return Err("up: only with normal".into());
        }
        Ok((d, r, u, [dot(p3, r), dot(p3, u)]))
    }
}

impl Eval for Project {
    fn eval(&self, cx: &Cx) -> Result<Outs, String> {
        let (d, r, u, at) = self.frame()?;
        let pos = cx.s.pos.clone();
        let nrm = cx.s.nrm.clone();
        let (Buf::V3(pos), Buf::V3(nrm)) = (&*pos, &*nrm) else {
            return Err("project needs positions and normals".into());
        };
        let n = cx.n();
        let valid = cx.s.valid.clone();
        let front = (0..n)
            .filter(|&i| valid.as_ref().is_none_or(|v| v[i]))
            .map(|i| dot(pos[i], d))
            .fold(f32::MIN, f32::max);
        let (lo, hi) = (self.lo.unwrap_or(0.6), self.hi.unwrap_or(0.85));
        let depth = self.depth;
        Ok(vec![
            (
                "",
                pv2(n, |i| [dot(pos[i], r) - at[0], -(dot(pos[i], u) - at[1])]),
            ),
            (
                "facing",
                pf(n, |i| {
                    let c = dot(nrm[i], d);
                    let t = ((c - lo) / (hi - lo).max(1e-6)).clamp(0.0, 1.0);
                    let w = t * t * (3.0 - 2.0 * t);
                    match depth {
                        Some(dd) if front - dot(pos[i], d) > dd => 0.0,
                        _ => w,
                    }
                }),
            ),
        ])
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::material::eval::{Samples, evaluate};
    use crate::material::node::In;

    fn port(nodes: serde_json::Value, s: &Samples, name: &str) -> Vec<f32> {
        let serde_json::Value::Array(nodes) = nodes else {
            panic!("array")
        };
        let ev = evaluate(&nodes, &Default::default(), s, false).unwrap_or_else(|e| panic!("{e}"));
        let f = ev
            .field(&In::Name(name.into()))
            .unwrap_or_else(|e| panic!("{e}"));
        (0..s.len()).map(|i| f.f(i)).collect()
    }

    const ART: &str = r##"<svg viewBox="0 0 100 100"><rect id="box" x="10" y="10" width="40" height="20" fill="#ff0000"/><circle id="dot" cx="70" cy="70" r="20" fill="#0000ff" fill-opacity="0.5"/></svg>"##;

    #[test]
    fn svg_sdf_matches_analytic_distances() {
        // 100 units -> 1 m: a unit is a centimetre; tile 1 m, 64 texels.
        let s = Samples::tile(64, 1.0);
        let nodes = json!([{"type": "svg", "name": "logo", "svg": ART, "size": 1.0}]);
        let d = port(nodes.clone(), &s, "logo");
        let box_d = port(nodes.clone(), &s, "logo.box");
        let dot_d = port(nodes, &s, "logo.dot");
        let uv = |i: usize| {
            let (x, y) = (i % 64, i / 64);
            [(x as f32 + 0.5) / 64.0, (y as f32 + 0.5) / 64.0]
        };
        for i in 0..s.len() {
            let p = uv(i);
            // Rect 0.1..0.5 x 0.1..0.3.
            let q = [(p[0] - 0.3).abs() - 0.2, (p[1] - 0.2).abs() - 0.1];
            let out = (q[0].max(0.0).powi(2) + q[1].max(0.0).powi(2)).sqrt();
            let rect = out + q[0].max(q[1]).min(0.0);
            let circ = ((p[0] - 0.7).powi(2) + (p[1] - 0.7).powi(2)).sqrt() - 0.2;
            assert!(
                (box_d[i] - rect).abs() < 1e-3,
                "rect at {p:?}: {} vs {rect}",
                box_d[i]
            );
            assert!(
                (dot_d[i] - circ).abs() < 2e-3,
                "circle at {p:?}: {} vs {circ}",
                dot_d[i]
            );
            if rect.min(circ) > 0.0 {
                assert!((d[i] - rect.min(circ)).abs() < 2e-3);
            }
        }
    }

    #[test]
    fn svg_colors_and_alpha() {
        let s = Samples::tile(32, 1.0);
        let serde_json::Value::Array(nodes) =
            json!([{"type": "svg", "name": "logo", "svg": ART, "size": [1.0, 1.0]}])
        else {
            panic!()
        };
        let ev = evaluate(&nodes, &Default::default(), &s, false).unwrap_or_else(|e| panic!("{e}"));
        let col = ev
            .field(&In::Name("logo.colors".into()))
            .unwrap_or_else(|e| panic!("{e}"));
        let a = ev
            .field(&In::Name("logo.alpha".into()))
            .unwrap_or_else(|e| panic!("{e}"));
        let at = |x: f32, y: f32| ((y * 32.0) as usize) * 32 + (x * 32.0) as usize;
        let i = at(0.3, 0.2);
        assert!(col.v3(i)[0] > 0.99 && col.v3(i)[2] < 0.01 && a.f(i) > 0.99);
        let j = at(0.7, 0.7);
        assert!(col.v3(j)[2] > 0.99 && (a.f(j) - 0.5).abs() < 0.01);
        assert!(a.f(at(0.9, 0.1)) < 0.01);
    }

    #[test]
    fn project_lands_on_the_named_face_only() {
        let s = Samples::sphere(48, 0.5);
        let w = port(
            json!([{"type": "project", "name": "p", "face": "front"}]),
            &s,
            "p.facing",
        );
        let serde_json::Value::Array(nodes) =
            json!([{"type": "project", "name": "p", "face": "right", "at": [0.1, 0.2]}])
        else {
            panic!()
        };
        let ev = evaluate(&nodes, &Default::default(), &s, false).unwrap_or_else(|e| panic!("{e}"));
        let uv = ev
            .field(&In::Name("p".into()))
            .unwrap_or_else(|e| panic!("{e}"));
        let crate::material::Buf::V3(pos) = &*s.pos else {
            panic!()
        };
        let crate::material::Buf::V3(nrm) = &*s.nrm else {
            panic!()
        };
        for i in 0..s.len() {
            if nrm[i][2] < 0.5 {
                assert_eq!(w[i], 0.0, "only +z-facing samples");
            }
            if nrm[i][2] > 0.9 {
                assert!(w[i] > 0.99);
            }
            // Right face: u = -(z - 0.1) (seen from +x, at z 0.1), v = -(y - 0.2).
            let c = uv.v2(i);
            assert!((c[0] + (pos[i][2] - 0.1)).abs() < 1e-5);
            assert!((c[1] + (pos[i][1] - 0.2)).abs() < 1e-5);
        }
        // depth: only the cap within 0.05 m of the front.
        let w = port(
            json!([{"type": "project", "name": "p", "face": "front", "depth": 0.05}]),
            &s,
            "p.facing",
        );
        for i in 0..s.len() {
            if pos[i][2] < 0.44 {
                assert_eq!(w[i], 0.0);
            }
        }
    }
}
