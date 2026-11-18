//! SVG artwork: parsed with `usvg` (transforms, groups, basic shapes,
//! paths, strokes converted to outlines, text converted to glyph paths with
//! the bundled Fira fonts), curves flattened adaptively into closed rings.
//! Feeds the material `svg` node (an exact signed distance field per shape,
//! bucketed in an [`EdgeGrid`]) and svg profiles of extrude / revolve /
//! sweep / loft / silhouette.
//!
//! Coordinates stay in SVG user units, y down; callers map them.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};

use usvg::tiny_skia_path::{PathSegment, Transform};

use crate::material::sdf::EdgeGrid;

/// One filled outline: a path's fill, or a stroke turned into its outline.
pub struct Shape {
    /// The element's own id and every enclosing group's id.
    pub ids: Vec<String>,
    /// Closed rings in SVG user units (y down).
    pub rings: Vec<Vec<[f32; 2]>>,
    /// Fill rule: nonzero (true) or even-odd.
    pub nonzero: bool,
    /// sRGB colour 0..1 and alpha (fill / stroke opacity times the groups').
    pub color: [f32; 4],
    pub grid: EdgeGrid,
    pub lo: [f32; 2],
    pub hi: [f32; 2],
}

/// A parsed drawing.
pub struct Art {
    /// The document box: (x, y) of its top-left corner and its size, user
    /// units (the viewBox for a full document, the content's bounds for a
    /// fragment).
    pub origin: [f32; 2],
    pub size: [f32; 2],
    /// Painter's order.
    pub shapes: Vec<Shape>,
    /// Every element id that owns a shape, first-seen order.
    pub ids: Vec<String>,
}

impl Art {
    /// The bounds of the shapes owned by `id` (all shapes with `None`).
    pub fn bounds(&self, id: Option<&str>) -> Option<([f32; 2], [f32; 2])> {
        let mut lo = [f32::MAX; 2];
        let mut hi = [f32::MIN; 2];
        let mut any = false;
        for s in self.shapes.iter().filter(|s| owned(s, id)) {
            any = true;
            lo = [lo[0].min(s.lo[0]), lo[1].min(s.lo[1])];
            hi = [hi[0].max(s.hi[0]), hi[1].max(s.hi[1])];
        }
        any.then_some((lo, hi))
    }

    /// Signed distance (user units, negative inside) to the union of the
    /// shapes owned by `id` (all with `None`).
    pub fn sdf(&self, p: [f32; 2], id: Option<&str>) -> f32 {
        let mut best = f32::MAX;
        for s in &self.shapes {
            if !owned(s, id) {
                continue;
            }
            let bd = box_dist(p, s.lo, s.hi);
            // Outside this shape's box it can't beat a nearer (or inside) hit.
            if bd > 0.0 && bd >= best {
                continue;
            }
            best = best.min(s.grid.sdf(p));
        }
        best
    }
}

fn owned(s: &Shape, id: Option<&str>) -> bool {
    id.is_none_or(|i| s.ids.iter().any(|x| x == i))
}

#[inline]
pub fn box_dist(p: [f32; 2], lo: [f32; 2], hi: [f32; 2]) -> f32 {
    let dx = (lo[0] - p[0]).max(p[0] - hi[0]).max(0.0);
    let dy = (lo[1] - p[1]).max(p[1] - hi[1]).max(0.0);
    (dx * dx + dy * dy).sqrt()
}

fn fontdb() -> Arc<usvg::fontdb::Database> {
    static DB: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        for f in [crate::text::SANS, crate::text::BOLD, crate::text::MONO] {
            db.load_font_data(f.to_vec());
        }
        db.set_sans_serif_family("Fira Sans");
        db.set_serif_family("Fira Sans");
        db.set_monospace_family("Fira Mono");
        db.set_cursive_family("Fira Sans");
        db.set_fantasy_family("Fira Sans");
        Arc::new(db)
    })
    .clone()
}

/// A fragment (`<path d=...>`, several elements) is wrapped in an `<svg>`.
fn is_fragment(src: &str) -> bool {
    let t = src.trim_start();
    let t = if t.starts_with("<?xml") {
        t.find("?>").map_or(t, |i| t[i + 2..].trim_start())
    } else {
        t
    };
    !(t.starts_with("<svg") || t.starts_with("<!DOCTYPE") || t.starts_with("<!--"))
}

type Cache = Mutex<Option<HashMap<u64, Arc<Art>>>>;
static CACHE: Cache = Mutex::new(None);

/// Parses SVG text (a document or a fragment of elements), cached by
/// content hash.
pub fn parse(src: &str) -> Result<Arc<Art>, String> {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    src.hash(&mut h);
    let key = h.finish();
    if let Ok(g) = CACHE.lock()
        && let Some(a) = g.as_ref().and_then(|m| m.get(&key))
    {
        return Ok(a.clone());
    }
    let art = Arc::new(parse_uncached(src)?);
    if let Ok(mut g) = CACHE.lock() {
        let m = g.get_or_insert_with(HashMap::new);
        if m.len() > 64 {
            m.clear();
        }
        m.insert(key, art.clone());
    }
    Ok(art)
}

fn parse_uncached(src: &str) -> Result<Art, String> {
    let fragment = is_fragment(src);
    let text = if fragment {
        format!("<svg xmlns=\"http://www.w3.org/2000/svg\">{src}</svg>")
    } else if !src.contains("xmlns=") {
        // usvg needs the namespace; hand-written snippets often skip it.
        src.replacen("<svg", "<svg xmlns=\"http://www.w3.org/2000/svg\"", 1)
    } else {
        src.to_string()
    };
    let opt = usvg::Options {
        fontdb: fontdb(),
        font_family: "Fira Sans".into(),
        ..Default::default()
    };
    let tree = usvg::Tree::from_str(&text, &opt).map_err(|e| format!("not valid SVG: {e}"))?;
    let size = tree.size();
    // Curves flatten to within this many user units: ~1/2000 of the drawing.
    let tol = (size.width().max(size.height()) / 2000.0).max(1e-4);
    let mut out = Vec::new();
    walk(tree.root(), &mut Vec::new(), 1.0, tol, &mut out);
    if out.is_empty() {
        return Err("the SVG has no filled or stroked shapes".into());
    }
    let mut ids: Vec<String> = Vec::new();
    for s in &out {
        for i in &s.ids {
            if !ids.contains(i) {
                ids.push(i.clone());
            }
        }
    }
    let (origin, size) = if fragment {
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for s in &out {
            lo = [lo[0].min(s.lo[0]), lo[1].min(s.lo[1])];
            hi = [hi[0].max(s.hi[0]), hi[1].max(s.hi[1])];
        }
        (lo, [(hi[0] - lo[0]).max(1e-6), (hi[1] - lo[1]).max(1e-6)])
    } else {
        ([0.0, 0.0], [size.width(), size.height()])
    };
    Ok(Art {
        origin,
        size,
        shapes: out,
        ids,
    })
}

fn walk(g: &usvg::Group, ids: &mut Vec<String>, opacity: f32, tol: f32, out: &mut Vec<Shape>) {
    let pushed = !g.id().is_empty();
    if pushed {
        ids.push(g.id().to_string());
    }
    let opacity = opacity * g.opacity().get();
    for n in g.children() {
        match n {
            usvg::Node::Group(c) => walk(c, ids, opacity, tol, out),
            usvg::Node::Path(p) => path_shapes(p, ids, opacity, tol, out),
            usvg::Node::Text(t) => {
                let pushed = !t.id().is_empty();
                if pushed {
                    ids.push(t.id().to_string());
                }
                walk(t.flattened(), ids, opacity, tol, out);
                if pushed {
                    ids.pop();
                }
            }
            usvg::Node::Image(_) => {}
        }
    }
    if pushed {
        ids.pop();
    }
}

fn paint_rgb(p: &usvg::Paint) -> [f32; 3] {
    let c = |c: usvg::Color| [c.red, c.green, c.blue].map(|v| v as f32 / 255.0);
    let stops = |s: &[usvg::Stop]| {
        if s.is_empty() {
            return [0.5; 3];
        }
        let mut a = [0.0; 3];
        for st in s {
            let k = c(st.color());
            a = [a[0] + k[0], a[1] + k[1], a[2] + k[2]];
        }
        a.map(|v| v / s.len() as f32)
    };
    match p {
        usvg::Paint::Color(k) => c(*k),
        usvg::Paint::LinearGradient(g) => stops(g.stops()),
        usvg::Paint::RadialGradient(g) => stops(g.stops()),
        usvg::Paint::Pattern(_) => [0.5; 3],
    }
}

fn path_shapes(p: &usvg::Path, ids: &[String], opacity: f32, tol: f32, out: &mut Vec<Shape>) {
    if !p.is_visible() {
        return;
    }
    let mut ids = ids.to_vec();
    if !p.id().is_empty() {
        ids.push(p.id().to_string());
    }
    let ts = p.abs_transform();
    let mut push = |path: &usvg::tiny_skia_path::Path, nonzero: bool, rgb: [f32; 3], a: f32| {
        let Some(path) = path.clone().transform(ts) else {
            return;
        };
        let rings = flatten(&path, tol);
        if rings.is_empty() {
            return;
        }
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for q in rings.iter().flatten() {
            lo = [lo[0].min(q[0]), lo[1].min(q[1])];
            hi = [hi[0].max(q[0]), hi[1].max(q[1])];
        }
        out.push(Shape {
            ids: ids.clone(),
            grid: EdgeGrid::with_rule(&rings, nonzero),
            rings,
            nonzero,
            color: [rgb[0], rgb[1], rgb[2], a],
            lo,
            hi,
        });
    };
    let fill_first = p.paint_order() == usvg::PaintOrder::FillAndStroke;
    let fill = |push: &mut dyn FnMut(&usvg::tiny_skia_path::Path, bool, [f32; 3], f32)| {
        if let Some(f) = p.fill() {
            push(
                p.data(),
                f.rule() == usvg::FillRule::NonZero,
                paint_rgb(f.paint()),
                f.opacity().get() * opacity,
            );
        }
    };
    let stroke = |push: &mut dyn FnMut(&usvg::tiny_skia_path::Path, bool, [f32; 3], f32)| {
        if let Some(s) = p.stroke() {
            let scale = transform_scale(ts).max(1e-6);
            if let Some(o) = p.data().stroke(&s.to_tiny_skia(), scale) {
                push(&o, true, paint_rgb(s.paint()), s.opacity().get() * opacity);
            }
        }
    };
    if fill_first {
        fill(&mut push);
        stroke(&mut push);
    } else {
        stroke(&mut push);
        fill(&mut push);
    }
}

fn transform_scale(t: Transform) -> f32 {
    ((t.sx * t.sy - t.kx * t.ky).abs()).sqrt()
}

/// Flattens a path into closed rings, curves split until each piece is
/// within `tol` of the curve.
pub fn flatten(path: &usvg::tiny_skia_path::Path, tol: f32) -> Vec<Vec<[f32; 2]>> {
    let mut rings: Vec<Vec<[f32; 2]>> = Vec::new();
    let mut cur: Vec<[f32; 2]> = Vec::new();
    let mut at = [0.0f32; 2];
    let finish = |cur: &mut Vec<[f32; 2]>, rings: &mut Vec<Vec<[f32; 2]>>| {
        let mut r = std::mem::take(cur);
        r.dedup_by(|a, b| (a[0] - b[0]).abs() < 1e-7 && (a[1] - b[1]).abs() < 1e-7);
        if r.len() > 2 {
            let (f, l) = (r[0], r[r.len() - 1]);
            if (f[0] - l[0]).abs() < 1e-7 && (f[1] - l[1]).abs() < 1e-7 {
                r.pop();
            }
        }
        if r.len() >= 3 {
            rings.push(r);
        }
    };
    let steps = |dev: f32| ((dev / tol).sqrt().ceil() as usize).clamp(1, 256);
    for seg in path.segments() {
        match seg {
            PathSegment::MoveTo(p) => {
                finish(&mut cur, &mut rings);
                at = [p.x, p.y];
                cur.push(at);
            }
            PathSegment::LineTo(p) => {
                at = [p.x, p.y];
                cur.push(at);
            }
            PathSegment::QuadTo(c, p) => {
                let (p0, c1, p2) = (at, [c.x, c.y], [p.x, p.y]);
                let dev = 0.25 * ((p0[0] - 2.0 * c1[0] + p2[0]).hypot(p0[1] - 2.0 * c1[1] + p2[1]));
                let n = steps(dev);
                for k in 1..=n {
                    let t = k as f32 / n as f32;
                    let u = 1.0 - t;
                    cur.push([
                        u * u * p0[0] + 2.0 * u * t * c1[0] + t * t * p2[0],
                        u * u * p0[1] + 2.0 * u * t * c1[1] + t * t * p2[1],
                    ]);
                }
                at = p2;
            }
            PathSegment::CubicTo(c1, c2, p) => {
                let (p0, a, b, p3) = (at, [c1.x, c1.y], [c2.x, c2.y], [p.x, p.y]);
                let d1 = (p0[0] - 2.0 * a[0] + b[0]).hypot(p0[1] - 2.0 * a[1] + b[1]);
                let d2 = (a[0] - 2.0 * b[0] + p3[0]).hypot(a[1] - 2.0 * b[1] + p3[1]);
                let n = steps(0.75 * d1.max(d2));
                for k in 1..=n {
                    let t = k as f32 / n as f32;
                    let u = 1.0 - t;
                    let (w0, w1, w2, w3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
                    cur.push([
                        w0 * p0[0] + w1 * a[0] + w2 * b[0] + w3 * p3[0],
                        w0 * p0[1] + w1 * a[1] + w2 * b[1] + w3 * p3[1],
                    ]);
                }
                at = p3;
            }
            PathSegment::Close => {
                finish(&mut cur, &mut rings);
                if let Some(r) = rings.last() {
                    at = r[0];
                }
            }
        }
    }
    finish(&mut cur, &mut rings);
    rings
}

/// A profile from SVG: the rings of every shape (or those owned by `id`),
/// fitted into meters. `size` [w, h] stretches the content's bounds to it
/// (`w` alone keeps the aspect), `height` sets the height (aspect kept);
/// with neither, 1 user unit = 1 mm. The content is centred on `center`;
/// `flip_y` (default true) turns SVG's y-down into profile b-up. Holes
/// come through as rings (even-odd, like every profile).
pub fn profile_rings(
    src: &str,
    id: Option<&str>,
    size: Option<[f32; 2]>,
    width_only: bool,
    height: Option<f32>,
    center: [f32; 2],
    flip_y: bool,
) -> Result<Vec<Vec<[f32; 2]>>, String> {
    let art = parse(src)?;
    if let Some(i) = id
        && !art.ids.iter().any(|x| x == i)
    {
        return Err(format!(
            "id: no element `{i}` (ids: {})",
            if art.ids.is_empty() {
                "none".to_string()
            } else {
                art.ids.join(", ")
            }
        ));
    }
    let Some((lo, hi)) = art.bounds(id) else {
        return Err("the SVG has no shapes".into());
    };
    let (w, h) = ((hi[0] - lo[0]).max(1e-9), (hi[1] - lo[1]).max(1e-9));
    let (sx, sy) = match (size, height) {
        (Some(_), Some(_)) => return Err("give size or height, not both".into()),
        (Some([a, _]), None) if width_only => (a / w, a / w),
        (Some([a, b]), None) => (a / w, b / h),
        (None, Some(t)) => (t / h, t / h),
        (None, None) => (0.001, 0.001),
    };
    if !(sx > 0.0 && sy > 0.0 && sx.is_finite() && sy.is_finite()) {
        return Err("size / height must be positive".into());
    }
    let mid = [(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5];
    let mut rings = Vec::new();
    for s in art.shapes.iter().filter(|s| owned(s, id)) {
        for r in &s.rings {
            rings.push(
                r.iter()
                    .map(|p| {
                        let x = (p[0] - mid[0]) * sx;
                        let y = (p[1] - mid[1]) * sy;
                        [center[0] + x, center[1] + if flip_y { -y } else { y }]
                    })
                    .collect(),
            );
        }
    }
    Ok(rings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_and_circle_distances_are_exact() {
        let art = parse(
            r#"<svg viewBox="0 0 100 100"><rect id="r" x="10" y="10" width="30" height="20"/><circle id="c" cx="70" cy="70" r="10"/></svg>"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(art.size, [100.0, 100.0]);
        assert!((art.sdf([25.0, 20.0], Some("r")) + 10.0).abs() < 1e-3);
        assert!((art.sdf([50.0, 20.0], Some("r")) - 10.0).abs() < 1e-3);
        assert!((art.sdf([70.0, 70.0], Some("c")) + 10.0).abs() < 0.06);
        assert!((art.sdf([90.0, 70.0], Some("c")) - 10.0).abs() < 0.06);
        // The union is the nearer of the two.
        assert!((art.sdf([75.0, 70.0], None) + 5.0).abs() < 0.06);
        assert_eq!(art.ids, vec!["r".to_string(), "c".to_string()]);
    }

    #[test]
    fn evenodd_and_nonzero_differ_on_a_ring() {
        // Two same-direction squares: nonzero fills the middle, even-odd cuts it.
        let d = "M0 0 H10 V10 H0 Z M3 3 H7 V7 H3 Z";
        let nz = parse(&format!(r#"<path d="{d}"/>"#)).unwrap_or_else(|e| panic!("{e}"));
        let eo = parse(&format!(r#"<path fill-rule="evenodd" d="{d}"/>"#))
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(nz.sdf([5.0, 5.0], None) < 0.0);
        assert!(eo.sdf([5.0, 5.0], None) > 0.0);
    }

    #[test]
    fn strokes_and_text_become_outlines() {
        let art = parse(
            r#"<svg viewBox="0 0 100 40"><line x1="5" y1="5" x2="95" y2="5" stroke="red" stroke-width="4"/><text id="t" x="10" y="35" font-size="20">HI</text></svg>"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(art.sdf([50.0, 5.0], None) < -1.5);
        assert!(art.ids.iter().any(|i| i == "t"));
        assert!(art.bounds(Some("t")).is_some());
    }
}
