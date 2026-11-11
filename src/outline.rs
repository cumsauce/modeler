//! The scene as a hierarchy (the outliner pane): parts grouped the way a
//! person thinks of them, not the way the op graph derives them.
//!
//! - prefab instances fold under the instance (`lamp1.pole` -> `lamp1`),
//! - array runs fold under their source (`book`, `book_1..13` -> `book ×14`),
//! - hidden parts (cutters, raw blanks, array sources) sink into one dimmed
//!   `hidden` group at the end, grouped the same way.
//!
//! Every row carries the part names it covers, so selecting a group selects
//! (highlights, frames) all of them.

use crate::{Model, Part};

/// One outliner row.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// Stable id (expansion state survives rebuilds): `part:x`, `prefab:x`,
    /// `array:x`, `hidden`.
    pub key: String,
    pub label: String,
    /// Material summary: the procedural material's name or the colour.
    pub detail: String,
    /// sRGB base colour for the swatch.
    pub swatch: [f32; 3],
    /// Every part this row stands for.
    pub parts: Vec<String>,
    pub children: Vec<Row>,
    /// Hidden parts (drawn dimmed).
    pub dim: bool,
}

/// `book_12` -> ("book", 12).
fn numbered(name: &str) -> Option<(&str, u32)> {
    let (base, n) = name.rsplit_once('_')?;
    if base.is_empty() || n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((base, n.parse().ok()?))
}

fn hex(c: [f32; 4]) -> String {
    // Part colours are stored as given (sRGB 0..1).
    let s = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", s(c[0]), s(c[1]), s(c[2]))
}

fn leaf(p: &Part, label: &str) -> Row {
    let m = &p.material;
    Row {
        key: format!("part:{}", p.name),
        label: label.to_string(),
        detail: match &m.pbr {
            Some(r) => format!("pbr {}", r.material),
            None => hex(m.base_color),
        },
        swatch: [m.base_color[0], m.base_color[1], m.base_color[2]],
        parts: vec![p.name.clone()],
        children: Vec::new(),
        dim: !p.visible,
    }
}

fn group(key: String, label: String, children: Vec<Row>, dim: bool) -> Row {
    let first = children.first();
    let detail = first.map(|c| c.detail.clone()).unwrap_or_default();
    let same = children.iter().all(|c| c.detail == detail);
    Row {
        key,
        label,
        detail: if same { detail } else { "mixed".into() },
        swatch: first.map_or([0.5; 3], |c| c.swatch),
        parts: children.iter().flat_map(|c| c.parts.clone()).collect(),
        children,
        dim,
    }
}

/// Groups `parts` (in model order) into prefab and array rows. `prefix` is
/// stripped from labels (a prefab's own parts read `pole`, not `lamp1.pole`).
fn rows(parts: &[&Part], prefix: &str, dim: bool) -> Vec<Row> {
    let label = |n: &str| n.strip_prefix(prefix).unwrap_or(n).to_string();
    let mut out: Vec<Row> = Vec::new();
    let mut done = vec![false; parts.len()];
    for i in 0..parts.len() {
        if done[i] {
            continue;
        }
        let name = parts[i].name.as_str();
        let local = &name[prefix.len().min(name.len())..];
        // Prefab instance: everything sharing the first dotted segment.
        if let Some((inst, _)) = local.split_once('.') {
            let head = format!("{prefix}{inst}.");
            let idx: Vec<usize> = (0..parts.len())
                .filter(|&j| !done[j] && parts[j].name.starts_with(&head))
                .collect();
            let members: Vec<&Part> = idx
                .iter()
                .map(|&j| {
                    done[j] = true;
                    parts[j]
                })
                .collect();
            let kids = rows(&members, &head, dim);
            let n = kids.iter().map(|k| k.parts.len()).sum::<usize>();
            out.push(group(
                format!("prefab:{prefix}{inst}{}", if dim { ":h" } else { "" }),
                format!("{inst} ({n})"),
                kids,
                dim,
            ));
            continue;
        }
        // Array run: `base` plus `base_N` copies (or two or more `base_N`).
        let base = numbered(name).map_or(name, |(b, _)| b);
        let run: Vec<usize> = (0..parts.len())
            .filter(|&j| {
                !done[j]
                    && (parts[j].name == base
                        || numbered(&parts[j].name).is_some_and(|(b, _)| b == base))
            })
            .collect();
        let copies = run.iter().filter(|&&j| parts[j].name != base).count();
        let has_base = run.iter().any(|&j| parts[j].name == base);
        if run.len() >= 2 && (has_base || copies >= 2) {
            let mut kids: Vec<Row> = run
                .iter()
                .map(|&j| {
                    done[j] = true;
                    leaf(parts[j], &label(&parts[j].name))
                })
                .collect();
            kids.sort_by_key(|r| numbered(r.parts[0].as_str()).map_or(0, |(_, n)| n as u64 + 1));
            out.push(group(
                format!("array:{prefix}{base}{}", if dim { ":h" } else { "" }),
                format!("{} ×{}", label(base), kids.len()),
                kids,
                dim,
            ));
            continue;
        }
        done[i] = true;
        out.push(leaf(parts[i], &label(name)));
    }
    out
}

/// The outliner tree for a model: visible parts first, then a `hidden` group.
pub fn build(model: &Model) -> Vec<Row> {
    let all = || {
        model
            .parts
            .iter()
            .map(|p| p.as_ref())
            .filter(|p| !p.verts.is_empty())
    };
    let shown: Vec<&Part> = all().filter(|p| p.visible).collect();
    let hidden: Vec<&Part> = all().filter(|p| !p.visible).collect();
    let mut out = rows(&shown, "", false);
    if !hidden.is_empty() {
        let kids = rows(&hidden, "", true);
        let n = hidden.len();
        let mut g = group("hidden".into(), format!("hidden ({n})"), kids, true);
        g.detail = "cutters, blanks, sources".into();
        out.push(g);
    }
    out
}

/// Rows in display order with their depth, descending only into groups whose
/// key `open` accepts.
pub fn flatten<'a>(rows: &'a [Row], open: &dyn Fn(&str) -> bool) -> Vec<(usize, &'a Row)> {
    fn walk<'a>(
        rows: &'a [Row],
        depth: usize,
        open: &dyn Fn(&str) -> bool,
        out: &mut Vec<(usize, &'a Row)>,
    ) {
        for r in rows {
            out.push((depth, r));
            if !r.children.is_empty() && open(&r.key) {
                walk(&r.children, depth + 1, open, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(rows, 0, open, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::Session;
    use serde_json::json;

    #[test]
    fn prefabs_arrays_and_hidden_parts_fold() {
        let mut s = Session::default();
        let items = json!([
            {"type": "primitive", "name": "shelf", "kind": "box", "size": [1, 0.02, 0.3]},
            {"type": "primitive", "name": "book", "kind": "box", "size": [0.03, 0.2, 0.15], "translation": [0, 0.11, 0]},
            {"type": "linear_array", "name": "book", "input": "book", "count": 4, "offset": [0.04, 0, 0]},
            {"type": "primitive", "name": "blank", "kind": "box", "size": 0.2},
            {"type": "primitive", "name": "cut", "kind": "box", "size": 0.1},
            {"type": "boolean", "name": "block", "op": "difference", "a": "blank", "b": "cut"},
            {"type": "hide_part", "part": "blank"},
            {"type": "hide_part", "part": "cut"},
            {"type": "material", "name": "shelf", "color": "#ff0000"}
        ]);
        s.apply_values_as(None, items.as_array().cloned().unwrap_or_default(), false)
            .unwrap_or_else(|e| panic!("{e}"));
        let t = build(&s.model);
        let labels: Vec<&str> = t.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(
            labels,
            ["shelf", "book ×4", "block", "hidden (2)"],
            "{t:#?}"
        );
        assert_eq!(t[0].detail, "#ff0000");
        assert_eq!(t[1].parts, ["book", "book_1", "book_2", "book_3"]);
        assert!(t[3].dim && t[3].parts.len() == 2);
        // Collapsed by default: only top rows; opening the array shows copies.
        assert_eq!(flatten(&t, &|_| false).len(), 4);
        let open = flatten(&t, &|k| k == "array:book");
        assert_eq!(open.len(), 8);
        assert_eq!((open[2].0, open[2].1.label.as_str()), (1, "book"));
    }

    #[test]
    fn prefab_instances_group_with_local_labels() {
        let mut m = Model::default();
        for n in ["lamp1.pole", "lamp1.shade", "lamp2.pole", "desk"] {
            let p = Part {
                name: n.to_string(),
                verts: vec![[0.0; 3]; 3],
                indices: vec![0, 1, 2],
                visible: true,
                ..Default::default()
            };
            m.parts.push(std::sync::Arc::new(p));
        }
        let t = build(&m);
        let labels: Vec<&str> = t.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(labels, ["lamp1 (2)", "lamp2 (1)", "desk"]);
        let kids: Vec<&str> = t[0].children.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(kids, ["pole", "shade"]);
        assert_eq!(t[0].parts, ["lamp1.pole", "lamp1.shade"]);
    }
}
