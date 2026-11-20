//! Packs MakeHuman's CC0 core assets into `data/human/human.bin`, the
//! template of the `body` preset `human`. Run by hand (not at build time):
//!
//! ```sh
//! cargo run --release --example pack_human -- <makehuman repo>/makehuman/data [out.bin]
//! ```
//!
//! It only parses data files (base.obj, *.target, default.mhskel,
//! default_weights.mhw, modifiers/*.json); nothing from the source tree is
//! executed. Prints the size per target group and what it dropped.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use modeler::human::asset::{Bone, Face, HumanAsset, Key, Target, Unit};

/// Mesh groups kept from base.obj (the rest are clothes and hair helpers,
/// eyelash cards and the joint cubes, which only feed the joint refs).
const KEEP: &[&str] = &[
    "body",
    "helper-l-eye",
    "helper-r-eye",
    "helper-upper-teeth",
    "helper-lower-teeth",
    "helper-tongue",
];

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(data) = args.get(1).map(PathBuf::from) else {
        eprintln!("usage: pack_human <makehuman/data dir> [out.bin]");
        std::process::exit(2);
    };
    let out = args
        .get(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("data/human/human.bin"));
    if let Err(e) = run(&data, &out) {
        eprintln!("pack_human: {e}");
        std::process::exit(1);
    }
}

fn read(p: &Path) -> Result<String, String> {
    std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))
}

fn json(p: &Path) -> Result<serde_json::Value, String> {
    serde_json::from_str(&read(p)?).map_err(|e| format!("{}: {e}", p.display()))
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut es: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    es.sort();
    for p in es {
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "target") {
            out.push(p);
        }
    }
}

fn run(data: &Path, out: &Path) -> Result<(), String> {
    // ---- mesh
    let obj = read(&data.join("3dobjs/base.obj"))?;
    let mut verts: Vec<[f32; 3]> = Vec::new();
    let mut uvs: Vec<[f32; 2]> = Vec::new();
    let mut group = String::new();
    let mut faces: Vec<(String, Vec<(u32, u32)>)> = Vec::new();
    for l in obj.lines() {
        let mut it = l.split_whitespace();
        match it.next() {
            Some("v") => {
                let p: Vec<f32> = it.filter_map(|x| x.parse().ok()).collect();
                if p.len() < 3 {
                    return Err(format!("bad vertex line `{l}`"));
                }
                // Decimeters -> meters.
                verts.push([p[0] * 0.1, p[1] * 0.1, p[2] * 0.1]);
            }
            Some("vt") => {
                let p: Vec<f32> = it.filter_map(|x| x.parse().ok()).collect();
                if p.len() < 2 {
                    return Err(format!("bad uv line `{l}`"));
                }
                uvs.push([p[0], p[1]]);
            }
            Some("g") => group = it.next().unwrap_or("").to_string(),
            Some("f") => {
                let mut f = Vec::new();
                for c in it {
                    let mut s = c.split('/');
                    let v: u32 = s.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                    let t: u32 = s.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                    if v == 0 || t == 0 {
                        return Err(format!("face corner `{c}` needs v/vt"));
                    }
                    f.push((v - 1, t - 1));
                }
                faces.push((group.clone(), f));
            }
            _ => {}
        }
    }
    // ---- skeleton
    let skel = json(&data.join("rigs/default.mhskel"))?;
    let jmap = skel["joints"].as_object().ok_or("mhskel: no joints")?;
    let bones_j = skel["bones"].as_object().ok_or("mhskel: no bones")?;
    // ---- kept vertices: kept faces + every joint ref
    let mut keep = vec![false; verts.len()];
    for (g, f) in &faces {
        if KEEP.contains(&g.as_str()) {
            for c in f {
                keep[c.0 as usize] = true;
            }
        }
    }
    for vs in jmap.values() {
        for v in vs.as_array().into_iter().flatten() {
            if let Some(i) = v.as_u64() {
                keep[i as usize] = true;
            }
        }
    }
    let mut remap = vec![u32::MAX; verts.len()];
    let mut a = HumanAsset {
        source: "MakeHuman core assets (makehuman/data), github.com/makehumancommunity/makehuman commit a8bc2d54, CC0 1.0".into(),
        ..HumanAsset::default()
    };
    for (i, v) in verts.iter().enumerate() {
        if keep[i] {
            remap[i] = a.verts.len() as u32;
            a.verts.push(*v);
        }
    }
    if a.verts.len() > 65535 {
        return Err("more than 65535 kept vertices".into());
    }
    let mut uv_keep: HashMap<u32, u32> = HashMap::new();
    let mut dropped_faces: BTreeMap<String, usize> = BTreeMap::new();
    for (g, f) in &faces {
        let Some(gi) = KEEP.iter().position(|k| k == g) else {
            *dropped_faces.entry(g.clone()).or_default() += 1;
            continue;
        };
        if f.len() != 4 {
            return Err(format!("group {g}: a {}-gon (quads only)", f.len()));
        }
        let mut face = Face {
            group: gi as u8,
            ..Face::default()
        };
        for (k, c) in f.iter().enumerate() {
            face.v[k] = remap[c.0 as usize] as u16;
            let n = uv_keep.len() as u32;
            let t = *uv_keep.entry(c.1).or_insert(n);
            if t == n {
                a.uvs.push(uvs[c.1 as usize]);
            }
            face.t[k] = t;
        }
        a.faces.push(face);
    }
    a.groups = KEEP.iter().map(|s| s.to_string()).collect();
    // Joints.
    let mut jnames: Vec<&String> = jmap.keys().collect();
    jnames.sort();
    let mut jidx: HashMap<String, u16> = HashMap::new();
    for n in jnames {
        let vs: Vec<u16> = jmap[n]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_u64())
            .map(|i| remap[i as usize] as u16)
            .collect();
        jidx.insert(n.clone(), a.joints.len() as u16);
        a.joints.push((n.clone(), vs));
    }
    // Bones: parents before children.
    let mut order: Vec<String> = Vec::new();
    let mut names: Vec<&String> = bones_j.keys().collect();
    names.sort();
    while order.len() < names.len() {
        let before = order.len();
        for n in &names {
            if order.contains(n) {
                continue;
            }
            let p = bones_j[*n]["parent"].as_str();
            if p.is_none_or(|p| order.iter().any(|o| o == p)) {
                order.push((*n).clone());
            }
        }
        if order.len() == before {
            return Err("mhskel: bone parents form a cycle".into());
        }
    }
    for n in &order {
        let b = &bones_j[n];
        let r = |k: &str| -> Result<u16, String> {
            let j = b[k].as_str().ok_or(format!("bone {n}: no {k}"))?;
            jidx.get(j)
                .copied()
                .ok_or(format!("bone {n}: unknown joint {j}"))
        };
        let parent = b["parent"]
            .as_str()
            .and_then(|p| order.iter().position(|o| o == p))
            .map_or(-1, |i| i as i16);
        a.bones.push(Bone {
            name: n.clone(),
            parent,
            head: r("head")?,
            tail: r("tail")?,
        });
    }
    // Weights.
    let wj = json(&data.join("rigs/default_weights.mhw"))?;
    let wmap = wj["weights"].as_object().ok_or("mhw: no weights")?;
    let mut dropped_w = 0usize;
    for b in &a.bones {
        let mut ws = Vec::new();
        for e in wmap
            .get(&b.name)
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
        {
            let (Some(i), Some(w)) = (e[0].as_u64(), e[1].as_f64()) else {
                continue;
            };
            let r = remap[i as usize];
            if r == u32::MAX {
                dropped_w += 1;
                continue;
            }
            ws.push((r as u16, w as f32));
        }
        a.weights.push(ws);
    }
    // ---- targets
    let troot = data.join("targets");
    let mut files = Vec::new();
    walk(&troot, &mut files);
    let mut lossy = 0usize;
    let mut zeros = 0usize;
    let mut off_mesh = 0usize;
    for f in &files {
        let rel = f
            .strip_prefix(&troot)
            .map_err(|e| e.to_string())?
            .with_extension("");
        let name = rel.to_string_lossy().replace('\\', "/");
        let mut t = Target {
            name,
            ..Target::default()
        };
        let mut rows: Vec<(u16, [i16; 3])> = Vec::new();
        for l in read(f)?.lines() {
            let l = l.trim();
            if l.is_empty() || l.starts_with('#') {
                continue;
            }
            let p: Vec<&str> = l.split_whitespace().collect();
            if p.len() < 4 {
                continue;
            }
            let Ok(i) = p[0].parse::<usize>() else {
                continue;
            };
            let d: Vec<f32> = p[1..4].iter().filter_map(|x| x.parse().ok()).collect();
            if d.len() < 3 || i >= remap.len() {
                continue;
            }
            if remap[i] == u32::MAX {
                off_mesh += 1;
                continue;
            }
            // Decimeters -> units of 0.1 mm.
            let q = [d[0], d[1], d[2]].map(|x| {
                let v = (x * 1000.0).round();
                if (v - x * 1000.0).abs() > 0.05 || v.abs() > 32767.0 {
                    lossy += 1;
                }
                v.clamp(-32767.0, 32767.0) as i16
            });
            if q == [0, 0, 0] {
                zeros += 1;
                continue;
            }
            rows.push((remap[i] as u16, [q[0], q[1], q[2]]));
        }
        rows.sort_by_key(|r| r.0);
        t.idx = rows.iter().map(|r| r.0).collect();
        t.d = rows.iter().map(|r| r.1).collect();
        a.targets.push(t);
    }
    let tindex: HashMap<String, usize> = a
        .targets
        .iter()
        .enumerate()
        .map(|(i, t)| (t.name.clone(), i))
        .collect();
    // Detail targets by file stem (expression units and macro files aside).
    let mut stem: HashMap<String, usize> = HashMap::new();
    for (i, t) in a.targets.iter().enumerate() {
        if t.name.starts_with("macrodetails/") || t.name.starts_with("expression/") {
            continue;
        }
        let s = t.name.rsplit('/').next().unwrap_or("").to_string();
        stem.insert(s, i);
    }
    // ---- keys from the modifier files
    let mut used = vec![false; a.targets.len()];
    let mut pairs: Vec<(String, Option<String>, Option<String>)> = Vec::new();
    for f in [
        "modeling_modifiers.json",
        "measurement_modifiers.json",
        "bodyshapes_modifiers.json",
    ] {
        let j = json(&data.join("modifiers").join(f))?;
        for g in j.as_array().into_iter().flatten() {
            for m in g["modifiers"].as_array().into_iter().flatten() {
                let Some(t) = m["target"].as_str() else {
                    continue;
                };
                pairs.push((
                    t.to_string(),
                    m["min"].as_str().map(String::from),
                    m["max"].as_str().map(String::from),
                ));
            }
        }
    }
    let mut count: HashMap<String, usize> = HashMap::new();
    for p in &pairs {
        *count.entry(p.0.clone()).or_default() += 1;
    }
    let mut missing = Vec::new();
    for (t, lo, hi) in &pairs {
        let find = |s: &str| stem.get(s).copied();
        let (low, high) = match (lo, hi) {
            (Some(l), Some(h)) => (find(&format!("{t}-{l}")), find(&format!("{t}-{h}"))),
            _ => (None, find(t)),
        };
        if low.is_none() && high.is_none() {
            missing.push(t.clone());
            continue;
        }
        let name = match (lo, hi) {
            (Some(l), Some(h)) if count[t] > 1 => format!("{t}-{l}-{h}"),
            _ => t.clone(),
        };
        for i in low.into_iter().chain(high) {
            used[i] = true;
        }
        a.keys.push(Key {
            name,
            low: low.map_or(-1, |i| i as i32),
            high: high.map_or(-1, |i| i as i32),
        });
    }
    // Detail targets no modifier names: one-sided keys by stem.
    let mut extra = 0;
    let mut stems: Vec<(&String, &usize)> = stem.iter().collect();
    stems.sort();
    for (s, &i) in stems {
        let breast_macro = a.targets[i].name.starts_with("breast/")
            && (s.starts_with("female-") || s.starts_with("male-"));
        if used[i] || breast_macro {
            continue;
        }
        if a.keys.iter().any(|k| &k.name == s) {
            continue;
        }
        a.keys.push(Key {
            name: s.clone(),
            low: -1,
            high: i as i32,
        });
        extra += 1;
    }
    // Expression units.
    let mut units: BTreeMap<String, [i32; 3]> = BTreeMap::new();
    for (i, t) in a.targets.iter().enumerate() {
        let p: Vec<&str> = t.name.split('/').collect();
        if p.len() == 4 && p[0] == "expression" && p[1] == "units" {
            let e = match p[2] {
                "african" => 0,
                "asian" => 1,
                _ => 2,
            };
            units.entry(p[3].to_string()).or_insert([-1; 3])[e] = i as i32;
        }
    }
    a.units = units
        .into_iter()
        .map(|(name, by_ethnicity)| Unit { name, by_ethnicity })
        .collect();
    let _ = tindex;
    // ---- write
    let file = a.encode();
    if let Some(d) = out.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(out, &file).map_err(|e| format!("{}: {e}", out.display()))?;
    // Check it reads back.
    let back = HumanAsset::decode(&file)?;
    if back.targets.len() != a.targets.len() || back.verts.len() != a.verts.len() {
        return Err("round trip mismatch".into());
    }
    // ---- report
    println!(
        "wrote {} ({:.2} MB): {} verts, {} uvs, {} quads, {} joint refs, {} bones, {} targets, {} keys ({} by stem only), {} expression units",
        out.display(),
        file.len() as f64 / 1e6,
        a.verts.len(),
        a.uvs.len(),
        a.faces.len(),
        a.joints.len(),
        a.bones.len(),
        a.targets.len(),
        a.keys.len(),
        extra,
        a.units.len()
    );
    let mut groups: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for t in &a.targets {
        let p: Vec<&str> = t.name.split('/').collect();
        let g = if p[0] == "macrodetails" && p.len() > 2 {
            format!("macrodetails/{}", p[1])
        } else if p[0] == "macrodetails" {
            if p[1].starts_with("universal") {
                "macrodetails/universal".into()
            } else {
                "macrodetails/ethnic".into()
            }
        } else if p[0] == "breast" && (p[1].starts_with("female-") || p[1].starts_with("male-")) {
            "breast (cup/firmness macro)".into()
        } else {
            p[0].to_string()
        };
        let e = groups.entry(g).or_default();
        e.0 += 1;
        e.1 += t.idx.len();
    }
    // Per group compressed estimate: the same columns compressed alone.
    println!("group | files | entries | ~compressed");
    for (g, (n, k)) in &groups {
        let sub = HumanAsset {
            targets: a
                .targets
                .iter()
                .filter(|t| {
                    let p: Vec<&str> = t.name.split('/').collect();
                    let tg = if p[0] == "macrodetails" && p.len() > 2 {
                        format!("macrodetails/{}", p[1])
                    } else if p[0] == "macrodetails" {
                        if p[1].starts_with("universal") {
                            "macrodetails/universal".into()
                        } else {
                            "macrodetails/ethnic".into()
                        }
                    } else if p[0] == "breast"
                        && (p[1].starts_with("female-") || p[1].starts_with("male-"))
                    {
                        "breast (cup/firmness macro)".into()
                    } else {
                        p[0].to_string()
                    };
                    &tg == g
                })
                .cloned()
                .collect(),
            ..HumanAsset::default()
        };
        println!(
            "{g} | {n} | {k} | {:.2} MB",
            sub.encode().len() as f64 / 1e6
        );
    }
    let base = HumanAsset {
        targets: Vec::new(),
        ..a.clone()
    };
    println!(
        "mesh + uvs + joints + skeleton + weights: {:.2} MB",
        base.encode().len() as f64 / 1e6
    );
    println!(
        "dropped: faces of groups {:?}; {dropped_w} weights on dropped vertices; {off_mesh} target rows on dropped vertices; {zeros} zero rows; {lossy} rows not exact at 0.1 mm; modifiers without files: {missing:?}",
        dropped_faces
    );
    Ok(())
}
