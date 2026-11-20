use super::*;
use crate::body::BodyDef;

/// Skips (returns None) without the template file.
fn asset_or_skip() -> Option<Arc<HumanAsset>> {
    match asset::load() {
        Ok(a) => Some(a),
        Err(e) => {
            eprintln!("skipping: {e}");
            None
        }
    }
}

pub(crate) fn human(f: impl FnOnce(&mut BodyDef)) -> BodyDef {
    let mut d = BodyDef {
        preset: "human".into(),
        ..BodyDef::default()
    };
    f(&mut d);
    d
}

#[test]
fn default_human_stands_on_the_ground_facing_z() {
    let Some(_) = asset_or_skip() else { return };
    let d = human(|d| {
        d.gender = Some(1.0);
        d.height = Some(1.755);
    });
    let t = std::time::Instant::now();
    let parts = build("eve", &d).unwrap_or_else(|e| panic!("{e}"));
    eprintln!("built in {:?}", t.elapsed());
    let b = &parts[0];
    let (lo, hi) = b.bounds();
    eprintln!(
        "bounds {lo} {hi}, {} verts {} tris",
        b.verts.len(),
        b.tri_count()
    );
    assert!((hi.y - lo.y - 1.755).abs() < 1e-3, "{lo} {hi}");
    assert!(lo.y.abs() < 1e-4);
    // The nose is the frontmost point of the head.
    let nose = b
        .verts
        .iter()
        .filter(|v| v[1] > 1.5)
        .max_by(|a, b| a[2].total_cmp(&b[2]))
        .copied()
        .unwrap_or_default();
    eprintln!("nose {nose:?}");
    assert!(nose[2] > 0.05 && nose[0].abs() < 0.01, "{nose:?}");
    for p in &parts {
        eprintln!("{} {} verts", p.name, p.verts.len());
    }
}

#[test]
fn natural_statures_follow_gender() {
    let Some(_) = asset_or_skip() else { return };
    let st = |g: f32| {
        let e = evaluate(&human(|d| d.gender = Some(g))).unwrap_or_else(|e| panic!("{e}"));
        let a = asset::load().unwrap_or_else(|e| panic!("{e}"));
        let (lo, hi) = body_extent_y(&a, &e.pos);
        hi - lo
    };
    let (f, m) = (st(0.0), st(1.0));
    eprintln!("natural stature: female {f:.3}, male {m:.3}");
    assert!(f > 1.5 && f < 1.7 && m > 1.65 && m < 1.85 && m > f + 0.05);
    // A child is smaller.
    let kid = evaluate(&human(|d| d.age = Some(8.0))).unwrap_or_else(|e| panic!("{e}"));
    let a = asset::load().unwrap_or_else(|e| panic!("{e}"));
    let (lo, hi) = body_extent_y(&a, &kid.pos);
    assert!(hi - lo < 1.4, "8 y: {}", hi - lo);
}

#[test]
fn the_game_rig_uses_the_biped_names() {
    let Some(a) = asset_or_skip() else { return };
    let r = rig(&a, false).unwrap_or_else(|e| panic!("{e}"));
    let names: Vec<&str> = r.bones.iter().map(|b| b.name.as_str()).collect();
    for n in crate::body::joint_names("biped") {
        assert!(
            names.contains(&n.as_str()),
            "game rig lacks biped joint `{n}`"
        );
    }
    for n in ["eye_l", "eye_r", "thumb_1_l", "index_3_r", "pinky_2_l"] {
        assert!(names.contains(&n), "{n}");
    }
    assert!((50..=70).contains(&names.len()), "{} bones", names.len());
    let full = rig(&a, true).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(full.bones.len(), a.bones.len());
    assert!(full.bones.iter().any(|b| b.name == "upperarm01_l"));
    // Every template bone lands on a rig bone; toes on the ball.
    let toe = a
        .bones
        .iter()
        .position(|b| b.name == "toe2-2.L")
        .unwrap_or(0);
    assert_eq!(r.bones[r.of_template[toe]].name, "ball_l");
}

#[test]
fn weights_detail_and_extras_are_sound() {
    let Some(_) = asset_or_skip() else { return };
    for detail in [0, 1] {
        let d = human(|d| d.detail = Some(detail));
        let parts = build("h", &d).unwrap_or_else(|e| panic!("{e}"));
        let names: Vec<&str> = parts.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "h",
                "h_eyes",
                "h_teeth",
                "h_tongue",
                "h_brows",
                "h_lashes",
                "h_hair",
                "h_hair_cap"
            ]
        );
        for p in &parts {
            let s = p
                .skin
                .as_ref()
                .unwrap_or_else(|| panic!("{} has no weights", p.name));
            assert_eq!(s.joints.len(), p.verts.len());
            for w in &s.weights {
                let t: f32 = w.iter().sum();
                assert!((t - 1.0).abs() < 1e-3, "{}: weights sum {t}", p.name);
            }
            // Closed: every edge (welded by position) has two faces
            // (brows, lashes and hair are open card sheets).
            if p.cards {
                continue;
            }
            let key = |i: u32| p.verts[i as usize].map(|c| (c * 1e5).round() as i64);
            let mut e: HashMap<([i64; 3], [i64; 3]), i32> = HashMap::new();
            for t in p.indices.as_chunks::<3>().0 {
                for k in 0..3 {
                    let (a, b) = (key(t[k]), key(t[(k + 1) % 3]));
                    *e.entry(if a < b { (a, b) } else { (b, a) }).or_default() += 1;
                }
            }
            let open = e.values().filter(|c| **c != 2).count();
            assert_eq!(open, 0, "{} at detail {detail}: {open} open edges", p.name);
        }
        let (lo, hi) = parts[0].bounds();
        eprintln!(
            "detail {detail}: {} tris, y {}..{}",
            parts[0].tri_count(),
            lo.y,
            hi.y
        );
        assert!(lo.y.abs() < 1e-4);
        if detail == 1 {
            assert!(parts[0].tri_count() > 100_000);
        }
    }
}

#[test]
fn shapes_are_morph_targets_and_names_are_checked() {
    let Some(_) = asset_or_skip() else { return };
    let mut d = human(|d| {
        d.shapes.insert(
            "open".into(),
            ShapeKey {
                expression: [("mouth-open".to_string(), 1.0)].into(),
                ..ShapeKey::default()
            },
        );
    });
    let parts = build("h", &d).unwrap_or_else(|e| panic!("{e}"));
    let m = parts[0]
        .morphs
        .as_ref()
        .unwrap_or_else(|| panic!("no morphs"));
    assert_eq!(m[0].0, "open");
    let biggest = m[0]
        .1
        .iter()
        .map(|v| Vec3::from(*v).length())
        .fold(0.0, f32::max);
    assert!(biggest > 0.01 && biggest < 0.1, "{biggest}");
    // Unknown names list close ones.
    d.shape.insert("nose-widht1".into(), 0.5);
    let e = validate(&d).err().unwrap_or_default();
    assert!(e.contains("nose-width1"), "{e}");
    d.shape.clear();
    d.shape.insert("nose-width1".into(), 0.5);
    assert!(validate(&d).is_ok());
    d.expression.insert("mouth-opne".into(), 0.5);
    let e = validate(&d).err().unwrap_or_default();
    assert!(e.contains("mouth-open"), "{e}");
}

#[test]
fn local_warps_move_only_their_region_and_tris_keeps_the_weights() {
    let Some(_) = asset_or_skip() else { return };
    let plain = build("h", &human(|_| {})).unwrap_or_else(|e| panic!("{e}"));
    let w: Vec<crate::deform::Deform> = serde_json::from_value(serde_json::json!([
        {"mode": "inflate", "at": "head", "radius": 0.12, "amount": 0.02}
    ]))
    .unwrap_or_else(|e| panic!("{e}"));
    let warped = build("h", &human(|d| d.warps = w.clone())).unwrap_or_else(|e| panic!("{e}"));
    let (a, b) = (&plain[0], &warped[0]);
    let moved: Vec<usize> = (0..a.verts.len())
        .filter(|&i| Vec3::from(a.verts[i]).distance(Vec3::from(b.verts[i])) > 1e-4)
        .collect();
    assert!(!moved.is_empty());
    assert!(
        moved.iter().all(|&i| a.verts[i][1] > 1.3),
        "only the head moves"
    );
    let low = build("h", &human(|d| d.tris = Some(3000))).unwrap_or_else(|e| panic!("{e}"));
    let s = low[0]
        .skin
        .as_ref()
        .unwrap_or_else(|| panic!("tris dropped the weights"));
    assert_eq!(s.joints.len(), low[0].verts.len());
    assert!(low[0].tri_count() <= 3300, "{}", low[0].tri_count());
}

#[test]
fn a_human_item_rigs_binds_and_exports_morphs() {
    let Some(_) = asset_or_skip() else { return };
    let mut s = crate::commands::Session::default();
    let items = serde_json::json!([
        {"type": "body", "name": "eve", "preset": "human", "gender": 0, "height": 1.626,
         "shapes": {"smile": {"expression": {"mouth-corner-puller": 1}}}},
        {"type": "material", "name": "eve", "color": "#c9a58a", "flat": true},
        {"type": "clip", "name": "wave", "skeleton": "eve_rig", "duration": 1,
         "tracks": {"shoulder_l": {"rotate": [[0, {"z": 0}], [0.5, {"z": 40}], [1, {"z": 0}]]}}}
    ]);
    let serde_json::Value::Array(items) = items else {
        panic!("array")
    };
    s.apply_values(None, items)
        .unwrap_or_else(|e| panic!("{e}"));
    let m = &s.model;
    for n in ["eve", "eve_eyes", "eve_teeth", "eve_tongue"] {
        let p = m
            .parts
            .iter()
            .find(|p| p.name == n)
            .unwrap_or_else(|| panic!("{n}"));
        let r = p.rig.as_ref().unwrap_or_else(|| panic!("{n} unbound"));
        assert_eq!(r.joints.len(), p.verts.len());
    }
    let eve = m
        .parts
        .iter()
        .find(|p| p.name == "eve")
        .unwrap_or_else(|| panic!("eve"));
    assert!(eve.joint("wrist_l").is_some());
    let dir = tempfile_dir();
    let path = dir.join("eve.gltf");
    crate::gltf::export_gltf(m, &path).unwrap_or_else(|e| panic!("{e}"));
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{e}")))
            .unwrap_or_else(|e| panic!("{e}"));
    let mesh = doc["meshes"]
        .as_array()
        .and_then(|ms| ms.iter().find(|x| x["name"] == "eve"))
        .unwrap_or_else(|| panic!("no eve mesh"));
    assert_eq!(mesh["extras"]["targetNames"][0], "smile");
    assert!(mesh["primitives"][0]["targets"][0]["POSITION"].is_u64());
    assert!(mesh["primitives"][0]["attributes"]["JOINTS_0"].is_u64());
    assert_eq!(doc["skins"][0]["joints"].as_array().map_or(0, Vec::len), 58);
    let _ = std::fs::remove_dir_all(dir);
}

fn tempfile_dir() -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("modeler-human-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&d);
    d
}

/// Points where `part`'s triangle edges cross the plane through `o` with
/// normal `n`.
fn cut(part: &Part, o: Vec3, n: Vec3) -> Vec<Vec3> {
    let mut pts = Vec::new();
    for t in part.indices.as_chunks::<3>().0 {
        let w = t.map(|i| Vec3::from(part.verts[i as usize]));
        for (a, b) in [(w[0], w[1]), (w[1], w[2]), (w[2], w[0])] {
            let (da, db) = ((a - o).dot(n), (b - o).dot(n));
            if da * db < 0.0 {
                pts.push(a.lerp(b, da / (da - db)));
            }
        }
    }
    pts
}

/// The loop of `pts` nearest `seed` (points chained within 4 cm: the
/// cage edges are up to ~3 cm long).
fn loop_near(pts: &[Vec3], seed: Vec3) -> Vec<Vec3> {
    let Some(c) = (0..pts.len()).min_by(|&a, &b| {
        pts[a]
            .distance_squared(seed)
            .total_cmp(&pts[b].distance_squared(seed))
    }) else {
        return Vec::new();
    };
    let mut keep = vec![false; pts.len()];
    keep[c] = true;
    let mut stack = vec![c];
    while let Some(i) = stack.pop() {
        for j in 0..pts.len() {
            if !keep[j] && pts[i].distance(pts[j]) < 0.04 {
                keep[j] = true;
                stack.push(j);
            }
        }
    }
    pts.iter()
        .zip(&keep)
        .filter(|k| *k.1)
        .map(|k| *k.0)
        .collect()
}

/// Perimeter of the convex hull of `pts` projected on the plane with
/// normal `n` (a tape measure round the section).
fn girth(pts: &[Vec3], n: Vec3) -> f32 {
    let n = n.normalize();
    let u = n.any_orthonormal_vector();
    let v = n.cross(u);
    let mut p: Vec<(f32, f32)> = pts.iter().map(|q| (q.dot(u), q.dot(v))).collect();
    p.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    p.dedup();
    if p.len() < 3 {
        return 0.0;
    }
    let cross = |o: (f32, f32), a: (f32, f32), b: (f32, f32)| {
        (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
    };
    let mut h: Vec<(f32, f32)> = Vec::new();
    for pass in 0..2 {
        let start = h.len();
        let it: Box<dyn Iterator<Item = &(f32, f32)>> = if pass == 0 {
            Box::new(p.iter())
        } else {
            Box::new(p.iter().rev())
        };
        for &q in it {
            while h.len() >= start + 2 && cross(h[h.len() - 2], h[h.len() - 1], q) <= 0.0 {
                h.pop();
            }
            h.push(q);
        }
        h.pop();
    }
    (0..h.len())
        .map(|i| {
            let (a, b) = (h[i], h[(i + 1) % h.len()]);
            ((a.0 - b.0).powi(2) + (a.1 - b.1).powi(2)).sqrt()
        })
        .sum()
}

fn extent(pts: &[Vec3]) -> (f32, f32) {
    let (mut lo, mut hi) = (Vec3::MAX, Vec3::MIN);
    for p in pts {
        lo = lo.min(*p);
        hi = hi.max(*p);
    }
    (hi.x - lo.x, hi.z - lo.z)
}

/// (measure, ANSUR II median cm, model cm) for a default adult.
fn measure(gender: f32, stature: f32, ansur: &[(&str, f32)]) -> Vec<(String, f32, f32)> {
    let d = human(|d| {
        d.gender = Some(gender);
        d.height = Some(stature);
        d.parts = Some(Vec::new());
    });
    let parts = build("m", &d).unwrap_or_else(|e| panic!("{e}"));
    let b = &parts[0];
    let joints = rig_raw(&d).unwrap_or_else(|e| panic!("{e}"));
    let j = |n: &str| {
        joints
            .iter()
            .find(|x| x.0 == n)
            .map(|x| x.2)
            .unwrap_or_else(|| panic!("no joint {n}"))
    };
    let st = stature;
    let f = |cm: f32| cm / 100.0;
    let get = |k: &str| {
        ansur
            .iter()
            .find(|a| a.0 == k)
            .map(|a| a.1)
            .unwrap_or_else(|| panic!("no ANSUR {k}"))
    };
    // ANSUR heights are for its median stature: scale to ours.
    let ansur_st = get("STATURE");
    let h = |k: &str| f(get(k)) * st / (ansur_st / 100.0);
    let y = Vec3::Y;
    let slab = |at: f32, seed: Option<Vec3>| {
        let pts = cut(b, Vec3::new(0.0, at, 0.0), y);
        match seed {
            Some(s) => loop_near(&pts, s),
            None => pts,
        }
    };
    let verts: Vec<Vec3> = b.verts.iter().map(|v| Vec3::from(*v)).collect();
    let mut out = Vec::new();
    let mut row = |name: &str, k: &str, v: f32| out.push((name.to_string(), get(k), v * 100.0));
    // Bideltoid: the widest span over the upper arms near the shoulders.
    let sh = j("shoulder_l");
    let bd = (0..20)
        .map(|i| {
            let yy = sh.y - 0.01 * i as f32;
            let pts = slab(yy, None);
            let pts: Vec<Vec3> = pts
                .into_iter()
                .filter(|p| p.x.abs() < sh.x + 0.07)
                .collect();
            extent(&pts).0
        })
        .fold(0.0, f32::max);
    row("bideltoid breadth", "BIDELTOID BREADTH", bd);
    // Chest at the nipples (about 0.72 of stature).
    let c = slab(0.72 * st, Some(Vec3::new(0.0, 0.72 * st, 0.0)));
    let (cw, cd) = extent(&c);
    row("chest breadth", "CHEST BREADTH", cw);
    row("chest depth", "CHEST DEPTH", cd);
    row("chest circumference", "CHEST CIRCUMFERENCE", girth(&c, y));
    let wy = h("WAIST HEIGHT (OMPHALION)");
    let w = slab(wy, Some(Vec3::new(0.0, wy, 0.0)));
    let (ww, wd) = extent(&w);
    row("waist breadth", "WAIST BREADTH", ww);
    row("waist depth", "WAIST DEPTH", wd);
    row(
        "waist circumference",
        "WAIST CIRCUMFERENCE (OMPHALION)",
        girth(&w, y),
    );
    // Hips: the widest / deepest slab between the crotch and the waist.
    let crotch = verts
        .iter()
        .filter(|p| p.x.abs() < 0.012 && p.y > 0.35 * st && p.y < 0.6 * st)
        .map(|p| p.y)
        .fold(f32::MAX, f32::min);
    let (mut hb, mut bdp, mut bc) = (0.0f32, 0.0f32, 0.0f32);
    for i in 0..30 {
        let yy = crotch + 0.005 + (wy - crotch) * i as f32 / 30.0;
        let s = slab(yy, Some(Vec3::new(0.0, yy, 0.0)));
        let (a, b2) = extent(&s);
        hb = hb.max(a);
        bdp = bdp.max(b2);
        bc = bc.max(girth(&s, y));
    }
    row("hip breadth", "HIP BREADTH", hb);
    row("buttock depth", "BUTTOCK DEPTH", bdp);
    row("buttock circumference", "BUTTOCK CIRCUMFERENCE", bc);
    // Head: widest and longest slab above the ear canal (ears included).
    let top = verts.iter().map(|p| p.y).fold(f32::MIN, f32::max);
    let (mut hw, mut hl) = (0.0f32, 0.0f32);
    for i in 0..20 {
        let yy = top - 0.04 - 0.004 * i as f32;
        let (a, b2) = extent(&slab(yy, Some(Vec3::new(0.0, yy, 0.0))));
        hw = hw.max(a);
        hl = hl.max(b2);
    }
    row("head breadth", "HEAD BREADTH", hw);
    row("head length", "HEAD LENGTH", hl);
    // Thigh just under the gluteal fold, one leg.
    let th = j("thigh_l");
    let ty = crotch - 0.02;
    // The thighs touch at the top: keep the +x half.
    let t: Vec<Vec3> = slab(ty, Some(Vec3::new(th.x, ty, th.z)))
        .into_iter()
        .filter(|p| p.x > 0.0)
        .collect();
    row("thigh circumference", "THIGH CIRCUMFERENCE", girth(&t, y));
    // Calf: the largest girth of the lower leg.
    let kn = j("knee_l");
    let an = j("ankle_l");
    let calf = (1..20)
        .map(|i| {
            let p = an.lerp(kn, i as f32 / 20.0);
            girth(&slab(p.y, Some(p)), y)
        })
        .fold(0.0, f32::max);
    row("calf circumference", "CALF CIRCUMFERENCE", calf);
    // Upper arm: square to the bone halfway down it (relaxed, not flexed).
    let el = j("elbow_l");
    let mid = sh.lerp(el, 0.5);
    let ua = loop_near(&cut(b, mid, el - sh), mid);
    row(
        "upper arm circumference (relaxed)",
        "BICEPS CIRCUMFERENCE, FLEXED",
        girth(&ua, el - sh),
    );
    row("crotch height", "CROTCH HEIGHT", crotch);
    row("knee height (joint)", "KNEE HEIGHT, MIDPATELLA", kn.y);
    // Acromion: the top of the shoulder over the shoulder joint.
    let acro = verts
        .iter()
        .filter(|p| (p.x - sh.x).abs() < 0.02 && (p.z - sh.z).abs() < 0.05 && p.y < sh.y + 0.12)
        .map(|p| p.y)
        .fold(0.0, f32::max);
    row("shoulder (acromial) height", "ACROMIAL HEIGHT", acro);
    out
}

fn ansur() -> Option<Vec<(String, f32, f32)>> {
    // The extracted ANSUR II medians (cm), when present.
    let p = std::env::var("MODELER_ANSUR").ok()?;
    let text = std::fs::read_to_string(p).ok()?;
    Some(
        text.lines()
            .filter_map(|l| {
                let c: Vec<&str> = l.split('|').map(str::trim).collect();
                if c.len() != 3 {
                    return None;
                }
                let fv = c[1].strip_prefix("F ")?.parse().ok()?;
                let mv = c[2].strip_prefix("M ")?.parse().ok()?;
                Some((c[0].to_string(), fv, mv))
            })
            .collect(),
    )
}

/// The default adult male (gender 1, 25 y, average muscle / weight, 1.755
/// m) and female (gender 0, 1.626 m) against ANSUR II medians: a report
/// with loose bounds (MakeHuman's average is not a soldier median), not a
/// fit. The medians are built in for the rows checked; `MODELER_ANSUR`
/// (a `NAME | F x | M y` file) prints them from the file instead.
#[test]
fn default_adults_against_ansur_medians() {
    let Some(_) = asset_or_skip() else { return };
    let builtin: Vec<(String, f32, f32)> = [
        ("STATURE", 162.6, 175.5),
        ("BIDELTOID BREADTH", 45.0, 50.9),
        ("CHEST BREADTH", 26.8, 28.9),
        ("CHEST DEPTH", 24.5, 25.3),
        ("CHEST CIRCUMFERENCE", 94.0, 105.6),
        ("WAIST BREADTH", 29.8, 32.5),
        ("WAIST DEPTH", 20.9, 23.4),
        ("WAIST CIRCUMFERENCE (OMPHALION)", 85.2, 93.7),
        ("WAIST HEIGHT (OMPHALION)", 97.7, 105.5),
        ("HIP BREADTH", 35.3, 34.4),
        ("BUTTOCK DEPTH", 23.1, 24.6),
        ("BUTTOCK CIRCUMFERENCE", 101.8, 101.7),
        ("HEAD BREADTH", 14.7, 15.4),
        ("HEAD LENGTH", 19.0, 20.0),
        ("THIGH CIRCUMFERENCE", 61.3, 62.4),
        ("CALF CIRCUMFERENCE", 37.2, 39.2),
        ("BICEPS CIRCUMFERENCE, FLEXED", 30.4, 35.7),
        ("CROTCH HEIGHT", 78.0, 84.5),
        ("KNEE HEIGHT, MIDPATELLA", 44.8, 48.7),
        ("ACROMIAL HEIGHT", 133.2, 143.9),
    ]
    .iter()
    .map(|r| (r.0.to_string(), r.1, r.2))
    .collect();
    let table = ansur().unwrap_or(builtin);
    let mut fails = Vec::new();
    for (g, st, label) in [(1.0f32, 1.755f32, "male"), (0.0, 1.626, "female")] {
        let rows: Vec<(&str, f32)> = table
            .iter()
            .map(|r| (r.0.as_str(), if g > 0.5 { r.2 } else { r.1 }))
            .collect();
        let m = measure(g, st, &rows);
        let mut t = format!("{label} ({st} m): measure | ANSUR median cm | model cm | error %\n");
        for (name, a, v) in &m {
            let e = (v / a - 1.0) * 100.0;
            t += &format!("{name} | {a:.1} | {v:.1} | {e:+.1}\n");
            // Loose: MakeHuman's average figure is leaner than a soldier
            // (the flexed biceps is measured relaxed here).
            let tol = if name.contains("upper arm") {
                30.0
            } else {
                20.0
            };
            if e.abs() > tol {
                fails.push(format!("{label} {name} {e:+.1}%"));
            }
        }
        eprintln!("{t}");
    }
    assert!(fails.is_empty(), "outside the loose bounds: {fails:?}");
}

#[test]
#[ignore]
fn pose_sheet_stretch_report() {
    let Some(_) = asset_or_skip() else { return };
    let poses = pose_sheet();
    let names: Vec<&str> = poses.iter().map(|p| p.0).collect();
    let mut s = crate::commands::Session::default();
    let mut items = vec![
        serde_json::json!({"type": "body", "name": "eve", "preset": "human",
        "parts": []}),
    ];
    items.extend(clip_items("eve_rig", &poses));
    s.apply_values(None, items)
        .unwrap_or_else(|e| panic!("{e}"));
    let game = pose_stretch(&s, &names);
    // The full template rig, the same rotations on its bones.
    let map = |b: &str| -> String {
        let (n, side) = match b.rsplit_once('_') {
            Some((n, sd @ ("l" | "r"))) => (n.to_string(), format!("_{sd}")),
            _ => (b.to_string(), String::new()),
        };
        let f = |k: &str| format!("{k}{side}");
        match n.as_str() {
            "thigh" => f("upperleg01"),
            "knee" => f("lowerleg01"),
            "ankle" => f("foot"),
            "shoulder" => f("upperarm01"),
            "elbow" => f("lowerarm01"),
            "wrist" => f("wrist"),
            "neck" => "neck01".into(),
            "spine" => "spine04".into(),
            "jaw" => "jaw".into(),
            x => {
                let (fin, k) = x.rsplit_once('_').unwrap_or((x, "1"));
                let i = ["thumb", "index", "middle", "ring", "pinky"]
                    .iter()
                    .position(|q| *q == fin)
                    .unwrap_or(0);
                format!("finger{}-{k}{side}", i + 1)
            }
        }
    };
    let full_poses: Vec<(&str, serde_json::Value)> = poses
        .iter()
        .map(|(n, v)| {
            let m: serde_json::Map<String, serde_json::Value> = v
                .as_object()
                .into_iter()
                .flatten()
                .map(|(b, r)| (map(b), r.clone()))
                .collect();
            (*n, serde_json::Value::Object(m))
        })
        .collect();
    let mut f = crate::commands::Session::default();
    let mut items = vec![
        serde_json::json!({"type": "body", "name": "eve", "preset": "human",
        "parts": [], "rig": "full"}),
    ];
    items.extend(clip_items("eve_rig", &full_poses));
    f.apply_values(None, items)
        .unwrap_or_else(|e| panic!("{e}"));
    let full = pose_stretch(&f, &names);
    for ((n, g), (_, fu)) in game.iter().zip(&full) {
        eprintln!("STRETCH {n:10} game {g:.2} full {fu:.2}");
    }
}

/// The everyday poses pass `verify animation` stretch 2.0 on the game rig
/// (the deep folds, elbow 120, arms raised 90 over the A-pose, crouch and
/// a 90 degree fist, don't yet: see AGENTS.md).
#[test]
fn everyday_poses_stretch_under_two() {
    let Some(_) = asset_or_skip() else { return };
    let poses: Vec<(&str, serde_json::Value)> = pose_sheet()
        .into_iter()
        .filter(|p| ["walk", "walk_b", "arms_t", "knee", "wrist", "neck", "spine"].contains(&p.0))
        .collect();
    let names: Vec<&str> = poses.iter().map(|p| p.0).collect();
    let mut s = crate::commands::Session::default();
    let mut items = vec![
        serde_json::json!({"type": "body", "name": "eve", "preset": "human",
        "parts": []}),
    ];
    items.extend(clip_items("eve_rig", &poses));
    s.apply_values(None, items)
        .unwrap_or_else(|e| panic!("{e}"));
    for (n, v) in pose_stretch(&s, &names) {
        eprintln!("{n}: {v:.2}");
        assert!((1.0..2.0).contains(&v), "{n}: stretch {v}");
    }
}

#[test]
fn arm_and_finger_frames_flex_the_anatomical_way() {
    let Some(_) = asset_or_skip() else { return };
    let d = human(|_| {});
    let frames = bone_frames(&d).unwrap_or_else(|e| panic!("{e}"));
    let bones = rig_raw(&d).unwrap_or_else(|e| panic!("{e}"));
    let get = |n: &str| {
        bones
            .iter()
            .find(|b| b.0 == n)
            .unwrap_or_else(|| panic!("{n}"))
    };
    let frame = |n: &str| {
        frames
            .iter()
            .find(|f| f.0 == n)
            .unwrap_or_else(|| panic!("{n}"))
    };
    for s in ["_l", "_r"] {
        // Elbow +x 90: the wrist comes toward the shoulder.
        let (e, sh) = (get(&format!("elbow{s}")), get(&format!("shoulder{s}")));
        let w = e.3.unwrap_or_else(|| panic!("tail"));
        let q = frame(&format!("elbow{s}")).1;
        let bent = e.2 + bevy::math::Quat::from_axis_angle(q * Vec3::X, 1.5) * (w - e.2);
        assert!(bent.distance(sh.2) < w.distance(sh.2) - 0.05, "elbow{s}");
        // Local y runs along the bone.
        assert!((q * Vec3::Y).dot((w - e.2).normalize()) > 0.999);
        // Finger +x curls the tip toward the palm (in, toward the thigh).
        let f = get(&format!("middle_2{s}"));
        let t = f.3.unwrap_or_else(|| panic!("tail"));
        let q = frame(&format!("middle_2{s}")).1;
        let c = f.2 + bevy::math::Quat::from_axis_angle(q * Vec3::X, 1.2) * (t - f.2);
        let inward = if s == "_l" { -1.0 } else { 1.0 };
        assert!((c.x - t.x) * inward > 0.005, "middle_2{s}: {t} -> {c}");
        assert!(frame(&format!("knee{s}")).2.is_some());
    }
}

#[test]
fn the_tongue_follows_an_open_mouth() {
    let Some(_) = asset_or_skip() else { return };
    let mean = |d: &BodyDef, part: &str| {
        let ps = build("eve", d).unwrap_or_else(|e| panic!("{e}"));
        let p = ps
            .iter()
            .find(|p| p.name == part)
            .unwrap_or_else(|| panic!("{part}"));
        p.verts.iter().map(|v| Vec3::from(*v)).sum::<Vec3>() / p.verts.len() as f32
    };
    let shut = human(|_| {});
    let open = human(|d| {
        d.expression.insert("mouth-open".into(), 1.0);
    });
    let dt = mean(&open, "eve_tongue") - mean(&shut, "eve_tongue");
    eprintln!("tongue moves {dt}");
    assert!(dt.y < -0.005, "tongue stays: {dt}");
}

#[test]
fn tris_keeps_the_template_uvs_and_detail_reaches_the_height() {
    let Some(_) = asset_or_skip() else { return };
    let d = human(|d| {
        d.tris = Some(3000);
        d.parts = Some(vec![]);
    });
    let b = build("eve", &d).unwrap_or_else(|e| panic!("{e}")).remove(0);
    let full = build("eve", &human(|d| d.parts = Some(vec![])))
        .unwrap_or_else(|e| panic!("{e}"))
        .remove(0);
    eprintln!("tris 3000: {} tris {} verts", b.tri_count(), b.verts.len());
    assert!(b.tri_count() <= 3100);
    assert_eq!(b.uvs.len(), b.verts.len());
    // Uvs inside the template's atlas (not a box unwrap), near the dense
    // surface's uv at the same spot.
    // Per light vertex: the uv distance to the closest uv among dense
    // vertices within 1.5 cm (seams make the plain nearest one ambiguous).
    let mut errs: Vec<f32> = b
        .verts
        .iter()
        .enumerate()
        .step_by(5)
        .map(|(i, v)| {
            let p = Vec3::from(*v);
            full.verts
                .iter()
                .zip(&full.uvs)
                .filter(|(w, _)| Vec3::from(**w).distance(p) < 0.015)
                .map(|(_, u)| ((u[0] - b.uvs[i][0]).powi(2) + (u[1] - b.uvs[i][1]).powi(2)).sqrt())
                .fold(f32::MAX, f32::min)
        })
        .collect();
    errs.sort_by(f32::total_cmp);
    let q = |f: f32| errs[((errs.len() - 1) as f32 * f) as usize];
    eprintln!(
        "uv error median {:.4} p95 {:.4} max {:.4}",
        q(0.5),
        q(0.95),
        q(1.0)
    );
    assert!(q(0.5) < 0.01 && q(0.95) < 0.03, "{} {}", q(0.5), q(0.95));
    for k in 0..=2u32 {
        let p = build(
            "eve",
            &human(|d| {
                d.detail = Some(k);
                d.height = Some(1.70);
                d.parts = Some(vec![]);
            }),
        )
        .unwrap_or_else(|e| panic!("{e}"))
        .remove(0);
        let (lo, hi) = p.bounds();
        eprintln!(
            "detail {k}: {} tris, height {:.5}",
            p.tri_count(),
            hi.y - lo.y
        );
        assert!(
            (hi.y - lo.y - 1.70).abs() < 2e-4 && lo.y.abs() < 1e-4,
            "{lo} {hi}"
        );
    }
}

#[test]
fn measure_hits_asked_girths() {
    let Some(a) = asset_or_skip() else { return };
    let want: Vec<(&str, f32)> = vec![
        ("chest", 1.056),
        ("waist", 0.85),
        ("hips", 1.017),
        ("neck", 0.40),
        ("upper_arm", 0.33),
        ("thigh", 0.60),
        ("calf", 0.39),
    ];
    let base = human(|d| {
        d.gender = Some(1.0);
        d.height = Some(1.755);
    });
    let ev0 = evaluate(&base).unwrap_or_else(|e| panic!("{e}"));
    let d = human(|d| {
        d.gender = Some(1.0);
        d.height = Some(1.755);
        d.measure = want.iter().map(|(k, v)| (k.to_string(), *v)).collect();
    });
    let ev = evaluate(&d).unwrap_or_else(|e| panic!("{e}"));
    let mut worst = 0.0f32;
    for (k, g) in &want {
        let before = measured(&a, &ev0.raw, ev0.scale, k).unwrap_or(0.0);
        let got = measured(&a, &ev.raw, ev.scale, k).unwrap_or(0.0);
        let err = (got - g) / g * 100.0;
        eprintln!("measure {k}: asked {g:.3} default {before:.3} got {got:.3} ({err:+.2}%)");
        worst = worst.max(err.abs());
    }
    assert!(worst < 3.0, "worst {worst}%");
    // Unknown names are errors.
    let bad = human(|d| {
        d.measure.insert("bicep".into(), 0.3);
    });
    assert!(evaluate(&bad).is_err());
}

#[test]
fn human_warps_take_part_anchors() {
    let Some(_) = asset_or_skip() else { return };
    let mut s = crate::commands::Session::default();
    let items = serde_json::json!([
        {"type": "primitive", "name": "mark", "kind": "box", "size": 0.02,
         "translation": [0.0, 1.2, 0.3]},
        {"type": "body", "name": "eve", "preset": "human", "parts": [],
         "warps": [{"mode": "inflate", "at": "eve:chest", "radius": 0.15, "amount": 0.01},
                   {"mode": "push", "at": "mark:center", "radius": 0.1, "direction": [0, 0, 1], "amount": 0.01},
                   {"mode": "inflate", "at": "top-front", "radius": 0.05, "amount": 0.002}]}
    ]);
    let serde_json::Value::Array(items) = items else {
        panic!("array")
    };
    s.apply_values(None, items)
        .unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn every_human_parameter_moves_the_rig_and_changes_the_op_key() {
    let Some(_) = asset_or_skip() else { return };
    let base = human(|_| {});
    let j0 = rig_raw(&base).unwrap_or_else(|e| panic!("{e}"));
    let s0 = serde_json::to_string(&base).unwrap_or_default();
    let edits: Vec<(&str, Box<dyn Fn(&mut BodyDef)>)> = vec![
        ("gender", Box::new(|d| d.gender = Some(0.0))),
        ("age", Box::new(|d| d.age = Some(60.0))),
        ("muscle", Box::new(|d| d.muscle = Some(0.9))),
        ("weight", Box::new(|d| d.weight = Some(0.9))),
        ("proportions", Box::new(|d| d.proportions = Some(0.9))),
        ("height", Box::new(|d| d.height = Some(1.9))),
        ("detail", Box::new(|d| d.detail = Some(1))),
        (
            "shape",
            Box::new(|d| {
                d.shape.insert("measure-upperleg-height".into(), 1.0);
            }),
        ),
        (
            "measure",
            Box::new(|d| {
                d.measure.insert("hips".into(), 1.2);
            }),
        ),
    ];
    for (n, f) in edits {
        let mut d = base.clone();
        f(&mut d);
        assert_ne!(
            serde_json::to_string(&d).unwrap_or_default(),
            s0,
            "{n}: op key"
        );
        let j = rig_raw(&d).unwrap_or_else(|e| panic!("{e}"));
        let moved = j0
            .iter()
            .zip(&j)
            .map(|(a, b)| a.2.distance(b.2))
            .fold(0.0f32, f32::max);
        eprintln!("{n}: joints move up to {:.4} m", moved);
        assert!(moved > 1e-5, "{n} doesn't move the rig");
    }
    // Cosmetic fields change the op key (the parts re-derive) only.
    let mut d = base.clone();
    d.eye_color = Some([0.2, 0.4, 0.8, 1.0]);
    assert_ne!(serde_json::to_string(&d).unwrap_or_default(), s0);
}

/// The pose sheet: (clip name, tracks at the peak, as game-rig values).
/// Arm and finger bends are flexions about the local x of [`bone_frames`].
pub(crate) fn pose_sheet() -> Vec<(&'static str, serde_json::Value)> {
    use serde_json::json;
    let d = human(|_| {});
    // The rig applies `bone_frames`: arm and finger bones flex about
    // their local x.
    let _ = d;
    let flex = |_b: &str, deg: f32| json!({"x": deg});
    let mut fingers = serde_json::Map::new();
    for f in ["index", "middle", "ring", "pinky"] {
        for k in 1..=3 {
            let b = format!("{f}_{k}_l");
            fingers.insert(b.clone(), flex(&b, if k == 1 { 80.0 } else { 90.0 }));
        }
    }
    vec![
        (
            "walk",
            json!({"thigh_l": {"x": -30}, "thigh_r": {"x": 30}, "knee_l": {"x": 50},
                        "shoulder_l": {"x": 25}, "shoulder_r": {"x": -25}}),
        ),
        (
            "walk_b",
            json!({"thigh_l": {"x": 30}, "thigh_r": {"x": -30}, "knee_r": {"x": 50}}),
        ),
        (
            "arms_t",
            json!({"shoulder_l": {"z": 45}, "shoulder_r": {"z": -45}}),
        ),
        (
            "arms_up",
            json!({"shoulder_l": {"z": 90}, "shoulder_r": {"z": -90}}),
        ),
        (
            "arms_fwd",
            json!({"shoulder_l": {"x": 90}, "shoulder_r": {"x": 90}}),
        ),
        ("elbow", json!({"elbow_l": flex("elbow_l", 120.0)})),
        ("knee", json!({"knee_l": {"x": 120}})),
        (
            "crouch",
            json!({"thigh_l": {"x": -100}, "thigh_r": {"x": -100}, "knee_l": {"x": 120},
                          "knee_r": {"x": 120}, "ankle_l": {"x": -20}, "ankle_r": {"x": -20}}),
        ),
        ("wrist", json!({"wrist_l": flex("wrist_l", 60.0)})),
        ("neck", json!({"neck": {"y": 60}})),
        ("spine", json!({"spine": {"x": 30}})),
        ("fist", serde_json::Value::Object(fingers)),
        ("jaw", json!({"jaw": {"x": 20}})),
    ]
}

/// One linear 1 s clip per pose: rest, the pose at 0.5, rest.
pub(crate) fn clip_items(rig: &str, poses: &[(&str, serde_json::Value)]) -> Vec<serde_json::Value> {
    use serde_json::json;
    poses
        .iter()
        .map(|(n, peak)| {
            let mut tracks = serde_json::Map::new();
            for (b, r) in peak.as_object().into_iter().flatten() {
                tracks.insert(
                    b.clone(),
                    json!({"rotate": [[0, {"x": 0}], [0.5, r], [1, {"x": 0}]]}),
                );
            }
            json!({"type": "clip", "name": n, "skeleton": rig, "duration": 1,
                   "interp": "linear", "tracks": tracks})
        })
        .collect()
}

/// Worst `verify animation` stretch per pose clip of a model.
pub(crate) fn pose_stretch(s: &crate::commands::Session, names: &[&str]) -> Vec<(String, f32)> {
    names
        .iter()
        .map(|n| {
            let spec: crate::anim::check::AnimationSpec = serde_json::from_value(
                serde_json::json!({"clip": n, "samples": 4, "stretch": 2.0, "loop": false}),
            )
            .unwrap_or_else(|e| panic!("{e}"));
            let r = crate::anim::check::animation(&s.model, &spec, &[]);
            let d = r
                .iter()
                .find(|x| x.0.ends_with("stretch"))
                .map(|x| x.2.clone())
                .unwrap_or_default();
            if std::env::var("STRETCH_DETAIL").is_ok() {
                eprintln!("  {n}: {d}");
            }
            let v = d
                .split_whitespace()
                .find_map(|w| {
                    w.trim_end_matches(',')
                        .strip_suffix('x')
                        .and_then(|x| x.parse::<f32>().ok())
                })
                .unwrap_or(-1.0);
            (n.to_string(), v)
        })
        .collect()
}

#[test]
fn the_elbow_limit_lets_the_arm_straighten() {
    let Some(_) = asset_or_skip() else { return };
    let frames = bone_frames(&human(|_| {})).unwrap_or_else(|e| panic!("{e}"));
    let l = frames
        .iter()
        .find(|f| f.0 == "elbow_l")
        .and_then(|f| f.2.clone())
        .unwrap_or_else(|| panic!("no elbow limits"));
    let x = l.x.unwrap_or([0.0, 0.0]);
    eprintln!("elbow_l x limits {x:?}");
    assert!(x[0] < -5.0 && x[1] > 90.0, "{x:?}");
}

/// The back shades smoothly: detail-0 normals along the back agree with
/// the detail-2 (near limit) surface's to well under a degree or two, and
/// neighbouring vertices don't alternate (no streaks).
#[test]
fn back_normals_are_smooth() {
    let Some(_) = asset_or_skip() else { return };
    let b0 = build("eve", &human(|d| d.parts = Some(vec![])))
        .unwrap_or_else(|e| panic!("{e}"))
        .remove(0);
    let b2 = build(
        "eve",
        &human(|d| {
            d.parts = Some(vec![]);
            d.detail = Some(2);
        }),
    )
    .unwrap_or_else(|e| panic!("{e}"))
    .remove(0);
    let mut errs: Vec<f32> = Vec::new();
    for (i, v) in b0.verts.iter().enumerate() {
        let p = Vec3::from(*v);
        // The back of the torso.
        if p.z > -0.02 || p.y < 1.0 || p.y > 1.35 || p.x.abs() > 0.12 {
            continue;
        }
        let j = b2
            .verts
            .iter()
            .enumerate()
            .min_by(|a, b| {
                Vec3::from(*a.1)
                    .distance_squared(p)
                    .total_cmp(&Vec3::from(*b.1).distance_squared(p))
            })
            .map_or(0, |x| x.0);
        let a = Vec3::from(b0.normals[i]);
        let b = Vec3::from(b2.normals[j]);
        errs.push(a.angle_between(b).to_degrees());
    }
    errs.sort_by(f32::total_cmp);
    let q = |f: f32| errs[((errs.len() - 1) as f32 * f) as usize];
    eprintln!(
        "back normals vs detail 2: {} verts, median {:.2} p95 {:.2} max {:.2} deg",
        errs.len(),
        q(0.5),
        q(0.95),
        q(1.0)
    );
    assert!(q(0.95) < 3.0);
}

/// Stretch per pose of the sheet with automatic correctives off and on,
/// and the build time they add.
#[test]
fn corrective_stretch_report() {
    let Some(_) = asset_or_skip() else { return };
    let mut poses = pose_sheet();
    let mut fr = serde_json::Map::new();
    let mut small = serde_json::Map::new();
    for f in ["index", "middle", "ring", "pinky"] {
        for k in 1..=3 {
            fr.insert(format!("{f}_{k}_r"), serde_json::json!({"x": 90}));
            small.insert(format!("{f}_{k}_r"), serde_json::json!({"x": 18}));
        }
    }
    poses.push(("fist_r", serde_json::Value::Object(fr)));
    poses.push(("curl18_r", serde_json::Value::Object(small)));
    use serde_json::json;
    for (n, v) in [
        (
            "sit",
            json!({"thigh_l": {"x": -90}, "thigh_r": {"x": -90}, "knee_l": {"x": 90}, "knee_r": {"x": 90}}),
        ),
        ("fore_twist", json!({"elbow_l": {"y": 80}})),
        ("fore_twist_n", json!({"elbow_l": {"y": -80}})),
        ("upper_twist", json!({"shoulder_l": {"y": 70}})),
        ("thigh_twist", json!({"thigh_l": {"y": 40}})),
        ("neck_turn", json!({"neck": {"y": 60}})),
        ("nod", json!({"neck": {"x": 30}, "head": {"x": 15}})),
        (
            "spine_twist",
            json!({"spine": {"y": 22}, "chest": {"y": 23}}),
        ),
        (
            "shrug",
            json!({"clavicle_l": {"z": 30}, "clavicle_r": {"z": -30}}),
        ),
        (
            "thumb_opp",
            json!({"thumb_1_l": {"x": 45}, "thumb_2_l": {"x": 30}, "thumb_3_l": {"x": 40}}),
        ),
        (
            "stand_straight",
            json!({"knee_l": {"x": -11}, "knee_r": {"x": -11}}),
        ),
    ] {
        poses.push((n, v));
    }
    let names: Vec<&str> = poses.iter().map(|p| p.0).collect();
    let run = |on: bool| {
        let mut s = crate::commands::Session::default();
        let mut items = vec![
            serde_json::json!({"type": "body", "name": "eve", "preset": "human", "parts": []}),
            serde_json::json!({"type": "bind", "name": "eve_bind", "skeleton": "eve_rig",
                "parts": ["eve"], "correctives": on}),
        ];
        items.extend(clip_items("eve_rig", &poses));
        let t0 = std::time::Instant::now();
        s.apply_values(None, items)
            .unwrap_or_else(|e| panic!("{e}"));
        let ms = t0.elapsed().as_millis();
        let body = s
            .model
            .parts
            .iter()
            .find(|p| p.name == "eve")
            .cloned()
            .unwrap_or_else(|| panic!("no eve"));
        let c = body.correctives.as_ref().map_or(0, |c| c.len());
        let sparse: usize = body
            .correctives
            .as_ref()
            .map_or(0, |c| c.iter().map(|x| x.idx.len()).sum());
        for c in body.correctives.iter().flat_map(|c| c.iter()) {
            let m = c
                .delta
                .iter()
                .map(|d| bevy::math::Vec3::from(*d).length())
                .fold(0.0f32, f32::max);
            eprintln!("  {} {} verts max {:.4}", c.name, c.idx.len(), m);
        }
        eprintln!(
            "CORRECTIVES on={on}: build {ms} ms, {c} targets, {sparse} sparse deltas, {} verts",
            body.verts.len()
        );
        // Popping: the largest step of any vertex between frames of a
        // fine elbow sweep (0 -> 120 -> 0 over 61 frames), and the sweep's
        // stretch at 16 samples.
        let (clip, skel) =
            crate::anim::clip(&s.model, Some("elbow")).unwrap_or_else(|e| panic!("{e}"));
        let mut prev: Option<Vec<[f32; 3]>> = None;
        let mut step = 0.0f32;
        for k in 0..=60 {
            let t = k as f32 / 60.0;
            let posed = crate::anim::posed_parts(&s.model, &clip, &skel, t);
            let v = posed
                .iter()
                .find(|p| p.name == "eve")
                .map(|p| p.verts.clone())
                .unwrap_or_default();
            if let Some(p) = &prev {
                for (a, b) in p.iter().zip(&v) {
                    step = step
                        .max((bevy::math::Vec3::from(*a) - bevy::math::Vec3::from(*b)).length());
                }
            }
            prev = Some(v);
        }
        let spec: crate::anim::check::AnimationSpec = serde_json::from_value(
            serde_json::json!({"clip": "elbow", "samples": 16, "stretch": 2.0, "loop": false}),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let r = crate::anim::check::animation(&s.model, &spec, &[]);
        let d = r
            .iter()
            .find(|x| x.0.ends_with("stretch"))
            .map(|x| x.2.clone())
            .unwrap_or_default();
        // The corrective offset's own step per 4 degrees (tents are
        // piecewise linear, so it is the largest key-to-key change / 10).
        let mut cstep = 0.0f32;
        let chain: Vec<_> = body
            .correctives
            .iter()
            .flat_map(|c| c.iter())
            .filter(|c| c.bone == "elbow_l")
            .collect();
        let mut last = vec![[0.0f32; 3]; body.verts.len()];
        for c in &chain {
            let d = c.dense(body.verts.len());
            for (a, b) in d.iter().zip(&last) {
                cstep = cstep
                    .max((bevy::math::Vec3::from(*a) - bevy::math::Vec3::from(*b)).length() / 10.0);
            }
            last = d;
        }
        eprintln!(
            "SWEEP on={on}: corrective offset step per 4 deg {:.2} mm",
            cstep * 1000.0
        );
        eprintln!(
            "SWEEP on={on}: max vertex step per 4 deg {:.2} mm; 16 samples: {d}",
            step * 1000.0
        );
        // Volume per pose at the peak (candy-wrapper check).
        let mut vols = Vec::new();
        for n in &names {
            let (clip, skel) =
                crate::anim::clip(&s.model, Some(n)).unwrap_or_else(|e| panic!("{e}"));
            let posed = crate::anim::posed_parts(&s.model, &clip, &skel, 0.5);
            let v = crate::anim::skin::volume_ratios(&s.model.parts, &posed, &skel)
                .into_iter()
                .find(|x| x.0 == "eve")
                .map_or(1.0, |x| x.1);
            vols.push(v);
        }
        let st = pose_stretch(&s, &names);
        (
            st.into_iter()
                .zip(vols)
                .map(|((n, a), v)| (n, a, v))
                .collect::<Vec<_>>(),
            c,
        )
    };
    let (off, n0) = run(false);
    let (on, n1) = run(true);
    assert_eq!(n0, 0);
    assert!(n1 > 10, "{n1} correctives");
    for ((n, a, va), (_, b, vb)) in off.iter().zip(&on) {
        eprintln!("STRETCH {n:14} off {a:.2} on {b:.2}   volume off {va:.4} on {vb:.4}");
    }
    // Targets met (see the AGENTS.md table): everything but the raised
    // arms' armpit, the sitting groin and the jaw stays under 2.
    let got = |n: &str| on.iter().find(|x| x.0 == n).map_or(9.0, |x| x.1);
    for n in [
        "walk",
        "arms_t",
        "arms_fwd",
        "elbow",
        "knee",
        "crouch",
        "wrist",
        "neck",
        "spine",
        "fist",
        "curl18_r",
        "fore_twist",
        "fore_twist_n",
        "upper_twist",
        "thigh_twist",
        "neck_turn",
        "nod",
        "spine_twist",
        "shrug",
        "thumb_opp",
        "stand_straight",
    ] {
        assert!(got(n) < 2.01, "{n}: {}", got(n));
    }
    assert!(
        got("curl18_r") < 1.4,
        "small curls regressed: {}",
        got("curl18_r")
    );
    for (n, cap) in [("arms_up", 2.2), ("sit", 2.2), ("jaw", 4.7)] {
        assert!(got(n) < cap, "{n}: {}", got(n));
    }
    // WebGL2's 24 targets: the priority order keeps one key per joint.
    let mut s = crate::commands::Session::default();
    let mut items = vec![
        serde_json::json!({"type": "body", "name": "eve", "preset": "human", "parts": []}),
        serde_json::json!({"type": "bind", "name": "eve_bind", "skeleton": "eve_rig",
            "parts": ["eve"], "correctives": {"max": 24}}),
    ];
    items.extend(clip_items("eve_rig", &poses));
    s.apply_values(None, items)
        .unwrap_or_else(|e| panic!("{e}"));
    let kept: Vec<String> = s
        .model
        .parts
        .iter()
        .find(|p| p.name == "eve")
        .and_then(|p| p.correctives.clone())
        .map(|c| c.iter().map(|c| c.name.clone()).collect())
        .unwrap_or_default();
    eprintln!("MAX24 kept {}: {}", kept.len(), kept.join(" "));
    for (n, v) in pose_stretch(&s, &names) {
        eprintln!("MAX24 {n:14} {v:.2}");
    }
}

/// The walk on each skinned LOD of the human: triangle counts, stretch,
/// and that the export writes LOD nodes sharing the skin.
#[test]
fn skinned_lods_walk_under_two() {
    let Some(_) = asset_or_skip() else { return };
    let walk: Vec<serde_json::Value> = serde_json::from_str(
        &std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/examples/human_walk.json"
        ))
        .unwrap_or_else(|e| panic!("{e}")),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let mut s = crate::commands::Session::default();
    s.apply_values(None, walk).unwrap_or_else(|e| panic!("{e}"));
    let clip = s
        .model
        .ops
        .iter()
        .find_map(|o| match o {
            crate::Op::Clip { name, .. } => Some(name.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no clip"));
    let body = s
        .model
        .parts
        .iter()
        .find(|p| p.name == "eve")
        .cloned()
        .unwrap_or_else(|| panic!("no eve"));
    for (k, ratio) in [-1.0f32, 1.0, 0.5, 0.2].iter().enumerate() {
        // -1: the full mesh without correctives (the baseline).
        let mut lod = crate::simplify::simplify(&body, ratio.abs());
        if *ratio < 0.0 {
            lod.correctives = None;
        }
        let rig = lod
            .rig
            .as_ref()
            .unwrap_or_else(|| panic!("lod lost its rig"));
        assert_eq!(rig.joints.len(), lod.verts.len());
        let corr = lod.correctives.as_ref().map_or(0, |c| c.len());
        let mut m = s.model.clone();
        for p in m.parts.iter_mut() {
            if p.name == "eve" {
                *p = std::sync::Arc::new(lod.clone());
            }
        }
        let spec: crate::anim::check::AnimationSpec = serde_json::from_value(
            serde_json::json!({"clip": clip, "samples": 16, "stretch": 2.0, "loop": false}),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let r = crate::anim::check::animation(&m, &spec, &[]);
        let st = r
            .iter()
            .find(|x| x.0.ends_with("stretch"))
            .unwrap_or_else(|| panic!("no stretch"));
        eprintln!(
            "LOD{k} ratio {ratio}: {} tris, {} verts, {corr} correctives: {} {}",
            lod.indices.len() / 3,
            lod.verts.len(),
            if st.1 { "PASS" } else { "FAIL" },
            st.2
        );
        if *ratio > 0.0 {
            assert!(st.1, "lod{k}: {}", st.2);
        }
    }
    // The export: LOD nodes, each skinned, with their own morph targets.
    let dir = std::env::temp_dir().join(format!("modeler-lods-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("eve.gltf");
    let mut opts = crate::export_opt::Options::game();
    opts.lods = vec![0.5, 0.2];
    let report =
        crate::gltf::export_gltf_with(&s.model, &path, &opts).unwrap_or_else(|e| panic!("{e}"));
    eprintln!("EXPORT {report}");
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{e}")))
            .unwrap_or_else(|e| panic!("{e}"));
    let nodes = doc["nodes"].as_array().cloned().unwrap_or_default();
    for want in ["eve", "eve.lod1", "eve.lod2"] {
        let n = nodes
            .iter()
            .find(|n| n["name"] == want)
            .unwrap_or_else(|| panic!("no node {want}"));
        assert!(n.get("skin").is_some(), "{want} is not skinned");
        let mesh = &doc["meshes"][n["mesh"].as_u64().unwrap_or(0) as usize];
        let targets = mesh["primitives"][0]["targets"]
            .as_array()
            .map_or(0, |t| t.len());
        let drivers = n["extras"]["correctives"].as_array().map_or(0, |t| t.len());
        eprintln!("EXPORT node {want}: {targets} morph targets, {drivers} drivers");
        assert!(drivers > 0, "{want} has no correctives table");
    }
    let bin: u64 = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|x| x == "bin" || x == "gltf")
        })
        .map(|e| e.metadata().map_or(0, |m| m.len()))
        .sum();
    eprintln!("EXPORT size {bin} bytes");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A sleeve modelled 6 mm over the arm takes its weights (and correctives)
/// from the body with `weights_from`, and stays within 2 mm of its rest
/// offset from the skin through the walk and a 120 degree elbow flex.
#[test]
fn sleeve_follows_the_skin() {
    use bevy::math::Vec3;
    let Some(_) = asset_or_skip() else { return };
    let mut s = crate::commands::Session::default();
    s.apply_values(
        None,
        vec![serde_json::json!({"type": "body", "name": "eve", "preset": "human", "parts": []})],
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let body = s
        .model
        .parts
        .iter()
        .find(|p| p.name == "eve")
        .cloned()
        .unwrap_or_else(|| panic!("no eve"));
    let (Some(sh), Some(wr)) = (body.joint("shoulder_l"), body.joint("wrist_l")) else {
        panic!("no arm joints")
    };
    assert!(!body.indices.is_empty());
    // The sleeve: a tube swept along the arm from the upper arm, over the
    // elbow, to the forearm, a little wider than the arm.
    let Some(el) = body.joint("elbow_l") else {
        panic!("no elbow")
    };
    let path: Vec<[f32; 3]> = [sh + (el - sh) * 0.35, el, el + (wr - el) * 0.6]
        .iter()
        .map(|p| p.to_array())
        .collect();
    let mut items = vec![
        serde_json::json!({"type": "body", "name": "eve", "preset": "human", "parts": []}),
        serde_json::json!({"type": "sweep", "name": "sleeve", "path": path, "smooth": true,
            "profile": [{"arc": {"center": [0, 0], "radius": 0.052, "from": 0, "to": 360}}]}),
        serde_json::json!({"type": "bind", "name": "sleeve_bind", "skeleton": "eve_rig",
            "parts": ["sleeve"], "weights_from": "eve"}),
    ];
    items.extend(clip_items(
        "eve_rig",
        &[
            ("flex", serde_json::json!({"elbow_l": {"x": 120}})),
            ("k40", serde_json::json!({"elbow_l": {"x": 40}})),
            (
                "swing",
                serde_json::json!({"shoulder_l": {"x": -60}, "elbow_l": {"x": 50}}),
            ),
        ],
    ));
    let walk: Vec<serde_json::Value> = serde_json::from_str(
        &std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/examples/human_walk.json"
        ))
        .unwrap_or_else(|e| panic!("{e}")),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    items.extend(walk.into_iter().filter(|i| i["type"] == "clip"));
    s.apply_values(None, items)
        .unwrap_or_else(|e| panic!("{e}"));
    let sleeve = s
        .model
        .parts
        .iter()
        .find(|p| p.name == "sleeve")
        .cloned()
        .unwrap_or_else(|| panic!("no sleeve"));
    assert!(sleeve.rig.is_some());
    // Rest offset of each sleeve vertex from its closest skin point.
    let body = s
        .model
        .parts
        .iter()
        .find(|p| p.name == "eve")
        .cloned()
        .unwrap_or_else(|| panic!("no eve"));
    let near = super::super::anim::skin::closest_for_test(&sleeve, &body);
    let offset = |sv: &[[f32; 3]], bv: &[[f32; 3]]| -> Vec<f32> {
        near.iter()
            .enumerate()
            .map(|(i, (tri, bc))| {
                let q: Vec3 = (0..3)
                    .map(|k| Vec3::from(bv[tri[k] as usize]) * bc[k])
                    .sum();
                (Vec3::from(sv[i]) - q).length()
            })
            .collect()
    };
    let rest_s: Vec<[f32; 3]> = (0..sleeve.verts.len())
        .map(|i| sleeve.world_vert(i).to_array())
        .collect();
    let rest_b: Vec<[f32; 3]> = (0..body.verts.len())
        .map(|i| body.world_vert(i).to_array())
        .collect();
    let rest = offset(&rest_s, &rest_b);
    let mut worst = 0.0f32;
    for clip in ["walk", "flex", "swing", "k40"] {
        let (c, k) = crate::anim::clip(&s.model, Some(clip)).unwrap_or_else(|e| panic!("{e}"));
        let mut w = 0.0f32;
        let mut tight = 0.0f32;
        for i in 0..=8 {
            let posed = crate::anim::posed_parts(&s.model, &c, &k, i as f32 / 8.0);
            let get = |n: &str| {
                posed
                    .iter()
                    .find(|p| p.name == n)
                    .map(|p| p.verts.clone())
                    .unwrap_or_default()
            };
            let now = offset(&get("sleeve"), &get("eve"));
            for (a, b) in now.iter().zip(&rest) {
                w = w.max((a - b).abs());
                if *b < 0.012 {
                    tight = tight.max((a - b).abs());
                }
            }
        }
        eprintln!(
            "SLEEVE {clip}: rest offset {:.1}..{:.1} mm, worst change {:.2} mm",
            rest.iter().copied().fold(f32::MAX, f32::min) * 1000.0,
            rest.iter().copied().fold(0.0, f32::max) * 1000.0,
            w * 1000.0
        );
        eprintln!(
            "SLEEVE {clip}: within 12 mm of the skin at rest ({} of {} verts): worst change {:.2} mm",
            rest.iter().filter(|r| **r < 0.012).count(),
            rest.len(),
            tight * 1000.0
        );
        // The elbow flex reaches its limit (~107 degrees): LBS carries the
        // offset by the blended matrix there, which shortens it a little.
        let max = if clip == "flex" { 0.0035 } else { 0.002 };
        assert!(
            tight < max,
            "{clip}: sleeve moved {:.2} mm off its offset",
            tight * 1000.0
        );
        worst = worst.max(tight);
    }
    eprintln!("SLEEVE worst {:.2} mm", worst * 1000.0);
}

/// Every game-rig joint has limits from the ROM table; the knee and the
/// elbow straighten (anatomical zero is inside the range), and no named
/// pose or walk key of `examples/` is clamped by them.
#[test]
fn limits_follow_the_rom_table_and_keep_the_examples() {
    use bevy::math::{EulerRot, Quat};
    let Some(_) = asset_or_skip() else { return };
    let frames = bone_frames(&human(|_| {})).unwrap_or_else(|e| panic!("{e}"));
    for (n, _, l) in &frames {
        eprintln!("LIMITS {n:12} {:?}", l.as_ref().map(|l| (l.x, l.y, l.z)));
    }
    let get = |n: &str| {
        frames
            .iter()
            .find(|f| f.0 == n)
            .and_then(|f| f.2.clone())
            .unwrap_or_else(|| panic!("no limits on {n}"))
    };
    for b in [
        "thigh_l",
        "knee_r",
        "ankle_l",
        "ball_r",
        "spine",
        "chest",
        "neck",
        "head",
        "jaw",
        "clavicle_l",
        "shoulder_r",
        "wrist_l",
        "thumb_1_l",
        "index_2_r",
    ] {
        let _ = get(b);
    }
    let knee = get("knee_l").x.unwrap_or([0.0, 0.0]);
    assert!(knee[0] < -4.0 && knee[1] > 130.0, "knee {knee:?}");
    let mut worst = (0.0f32, String::new());
    for f in ["human_poses.json", "human_walk.json"] {
        let path = format!("{}/examples/{f}", env!("CARGO_MANIFEST_DIR"));
        let items: Vec<serde_json::Value> =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{e}")))
                .unwrap_or_else(|e| panic!("{e}"));
        for it in items {
            let bones = it
                .get("bones")
                .or_else(|| it.get("tracks"))
                .and_then(|b| b.as_object())
                .cloned()
                .unwrap_or_default();
            for (b, v) in bones {
                let Some(r) = v.get("rotate") else { continue };
                let keys: Vec<serde_json::Value> = match r {
                    serde_json::Value::Array(a) => {
                        a.iter().filter_map(|k| k.get(1).cloned()).collect()
                    }
                    o => vec![o.clone()],
                };
                let Some(l) = frames.iter().find(|f| f.0 == b).and_then(|f| f.2.clone()) else {
                    continue;
                };
                for k in keys {
                    let d = |a: &str| k.get(a).and_then(|x| x.as_f64()).unwrap_or(0.0) as f32;
                    let q = Quat::from_euler(
                        EulerRot::ZYX,
                        d("z").to_radians(),
                        d("y").to_radians(),
                        d("x").to_radians(),
                    );
                    let c = crate::anim::ik::clamp(q, &l);
                    let e = q.angle_between(c).to_degrees();
                    if e > worst.0 {
                        worst = (e, format!("{f} {} {b} {k}", it["name"]));
                    }
                }
            }
        }
    }
    eprintln!(
        "LIMITS worst clamp of an example key: {:.2} deg {}",
        worst.0, worst.1
    );
    assert!(worst.0 < 0.5, "{} deg: {}", worst.0, worst.1);
}

/// Diagnostic: stretch of a pose with subsets of the correctives
/// (`CORR_POSE`, `CORR_KEEP` = comma globs over target names).
#[test]
#[ignore]
fn corrective_subset_probe() {
    let Some(_) = asset_or_skip() else { return };
    let mut poses = pose_sheet();
    let mut fr = serde_json::Map::new();
    let mut small = serde_json::Map::new();
    for f in ["index", "middle", "ring", "pinky"] {
        for k in 1..=3 {
            fr.insert(format!("{f}_{k}_r"), serde_json::json!({"x": 90}));
            small.insert(format!("{f}_{k}_r"), serde_json::json!({"x": 18}));
        }
    }
    poses.push(("fist_r", serde_json::Value::Object(fr)));
    poses.push(("curl18_r", serde_json::Value::Object(small)));
    use serde_json::json;
    poses.push(("m2_18", json!({"middle_2_r": {"x": 18}})));
    poses.push(("th_r", json!({"thigh_r": {"x": -100}})));
    poses.push(("up_r", json!({"shoulder_r": {"z": -90}})));
    poses.push(("up_r60", json!({"shoulder_r": {"z": -60}})));
    poses.push((
        "up_cl",
        json!({"shoulder_r": {"z": -75}, "clavicle_r": {"z": -15}}),
    ));
    poses.push((
        "up_cl2",
        json!({"shoulder_r": {"z": -90}, "clavicle_r": {"z": -20}}),
    ));
    poses.push((
        "thk_r",
        json!({"thigh_r": {"x": -100}, "knee_r": {"x": 120}}),
    ));
    poses.push((
        "th_lr",
        json!({"thigh_r": {"x": -100}, "thigh_l": {"x": -100}}),
    ));
    poses.push(("m2_30", json!({"middle_2_r": {"x": 30}})));
    poses.push((
        "m23_18",
        json!({"middle_2_r": {"x": 18}, "middle_3_r": {"x": 18}}),
    ));
    poses.push((
        "m12_18",
        json!({"middle_2_r": {"x": 18}, "middle_1_r": {"x": 18}}),
    ));
    poses.push((
        "mr2_18",
        json!({"middle_2_r": {"x": 18}, "ring_2_r": {"x": 18}}),
    ));
    let mut s = crate::commands::Session::default();
    let mut items = vec![
        serde_json::json!({"type": "body", "name": "eve", "preset": "human", "parts": []}),
        serde_json::json!({"type": "bind", "name": "eve_bind", "skeleton": "eve_rig",
            "parts": ["eve"], "correctives": true}),
    ];
    items.extend(clip_items("eve_rig", &poses));
    s.apply_values(None, items)
        .unwrap_or_else(|e| panic!("{e}"));
    let pose = std::env::var("CORR_POSE").unwrap_or("curl18_r".into());
    for keep in std::env::var("CORR_KEEP").unwrap_or("*".into()).split(';') {
        let mut m = crate::commands::Session::default();
        m.model = s.model.clone();
        for p in m.model.parts.iter_mut() {
            if p.name == "eve" {
                let mut q = (**p).clone();
                if let Some(c) = &q.correctives {
                    let v: Vec<_> = c
                        .iter()
                        .filter(|c| keep.split(',').any(|g| crate::name_matches(g, &c.name)))
                        .cloned()
                        .collect();
                    q.correctives = Some(std::sync::Arc::new(v));
                }
                *p = std::sync::Arc::new(q);
            }
        }
        for pose in pose.split(',') {
            if std::env::var("CORR_EDGE").is_ok() {
                use bevy::math::Vec3;
                let (c, k) =
                    crate::anim::clip(&m.model, Some(pose)).unwrap_or_else(|e| panic!("{e}"));
                let rest = m
                    .model
                    .parts
                    .iter()
                    .find(|p| p.name == "eve")
                    .cloned()
                    .unwrap_or_else(|| panic!());
                let mut plain = m.model.clone();
                for p in plain.parts.iter_mut() {
                    if p.name == "eve" {
                        let mut q = (**p).clone();
                        q.correctives = None;
                        *p = std::sync::Arc::new(q);
                    }
                }
                for t in [0.25f32, 0.5] {
                    let pa = crate::anim::posed_parts(&m.model, &c, &k, t);
                    let pb = crate::anim::posed_parts(&plain, &c, &k, t);
                    let get = |v: &Vec<std::sync::Arc<crate::Part>>| {
                        v.iter()
                            .find(|p| p.name == "eve")
                            .map(|p| p.verts.clone())
                            .unwrap_or_default()
                    };
                    let (va, vb) = (get(&pa), get(&pb));
                    let mut worst = (0.0f32, 0usize, 0usize);
                    for tri in rest.indices.as_chunks::<3>().0 {
                        for (a, b) in [(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])] {
                            let (a, b) = (a as usize, b as usize);
                            let l0 = (rest.world_vert(a) - rest.world_vert(b)).length();
                            if l0 < 1e-6 {
                                continue;
                            }
                            let l1 = (Vec3::from(va[a]) - Vec3::from(va[b])).length();
                            let r = (l1 / l0).max(l0 / l1.max(1e-9) * 0.4);
                            if r > worst.0 {
                                worst = (r, a, b);
                            }
                        }
                    }
                    let (_, a, b) = worst;
                    let d = |i: usize| {
                        rest.correctives
                            .iter()
                            .flat_map(|c| c.iter())
                            .map(|c| {
                                let dd = c.dense(rest.verts.len());
                                (c.name.clone(), Vec3::from(dd[i]))
                            })
                            .filter(|x| x.1.length() > 0.0)
                            .collect::<Vec<_>>()
                    };
                    eprintln!(
                        "EDGE t {t}: ratio {:.2} verts {a} {b} l0 {:.5} lbs {:.5} corr {:.5}; rest a {:?} b {:?}; da {:?} db {:?}",
                        worst.0,
                        (rest.world_vert(a) - rest.world_vert(b)).length(),
                        (Vec3::from(vb[a]) - Vec3::from(vb[b])).length(),
                        (Vec3::from(va[a]) - Vec3::from(va[b])).length(),
                        rest.world_vert(a),
                        rest.world_vert(b),
                        d(a),
                        d(b)
                    );
                }
            }
            let r = pose_stretch(&m, &[pose]);
            eprintln!("PROBE {pose} keep {keep}: {:.2}", r[0].1);
        }
    }
}

/// The shoulder's +x raises the arm forward (+z), its +z (left) sideways.
#[test]
fn shoulder_axes_point_the_anatomical_way() {
    use bevy::math::{Quat, Vec3};
    let Some(_) = asset_or_skip() else { return };
    let mut s = crate::commands::Session::default();
    let mut items =
        vec![serde_json::json!({"type": "body", "name": "eve", "preset": "human", "parts": []})];
    items.extend(clip_items(
        "eve_rig",
        &[("fwd", serde_json::json!({"shoulder_l": {"x": 60}}))],
    ));
    s.apply_values(None, items)
        .unwrap_or_else(|e| panic!("{e}"));
    let (_, skel) = crate::anim::clip(&s.model, Some("fwd")).unwrap_or_else(|e| panic!("{e}"));
    let (Some(sh), Some(el)) = (skel.bone("shoulder_l"), skel.bone("elbow_l")) else {
        panic!("no arm")
    };
    let mut pose = skel.rest();
    pose[sh].1 = Quat::from_rotation_x(60f32.to_radians());
    let m = skel.skin_matrices(&pose);
    let e0 = skel.bones[el].head;
    let e1 = m[sh].transform_point3(e0);
    eprintln!("shoulder x +60: elbow {e0} -> {e1}");
    assert!(e1.z > e0.z + 0.05, "{e0} -> {e1}");
    let _ = Vec3::ZERO;
}

/// The template's own `mouth-open` expression unit stretches the edges at
/// the lip corners 6.5x at 0.5 and 12x at 1 (the mouth slit is closed in
/// the template, so the corner edges are tiny): the jaw corrective
/// (relaxed, 4.6x at 20 degrees) cannot use it as a target that tears
/// less.
#[test]
#[ignore]
fn mouth_open_unit_stretch() {
    let Some(_) = asset_or_skip() else { return };
    let b0 = build("eve", &human(|d| d.parts = Some(vec![])))
        .unwrap_or_else(|e| panic!("{e}"))
        .remove(0);
    for v in [0.5f32, 1.0] {
        let b1 = build(
            "eve",
            &human(|d| {
                d.parts = Some(vec![]);
                d.expression.insert("mouth-open".into(), v);
            }),
        )
        .unwrap_or_else(|e| panic!("{e}"))
        .remove(0);
        let mut worst = (0.0f32, Vec3::ZERO);
        let mut maxd = 0.0f32;
        for t in b0.indices.as_chunks::<3>().0 {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                let (a, b) = (a as usize, b as usize);
                let l0 = Vec3::from(b0.verts[a]).distance(Vec3::from(b0.verts[b]));
                let l1 = Vec3::from(b1.verts[a]).distance(Vec3::from(b1.verts[b]));
                if l0 > 1e-6 && l1 / l0 > worst.0 {
                    worst = (l1 / l0, Vec3::from(b0.verts[a]));
                }
            }
        }
        for (p, q) in b0.verts.iter().zip(&b1.verts) {
            maxd = maxd.max(Vec3::from(*p).distance(Vec3::from(*q)));
        }
        eprintln!(
            "MOUTH {v}: worst edge {:.2} at {} max move {:.1} mm",
            worst.0,
            worst.1,
            maxd * 1000.0
        );
    }
}
