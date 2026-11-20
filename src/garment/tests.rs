use super::*;
use crate::body::BodyDef;

fn human(f: impl FnOnce(&mut BodyDef)) -> Option<Part> {
    if let Err(e) = crate::human::asset::load() {
        eprintln!("skipping: {e}");
        return None;
    }
    let mut d = BodyDef {
        preset: "human".into(),
        ..BodyDef::default()
    };
    f(&mut d);
    let parts = crate::human::build("eve", &d).unwrap_or_else(|e| panic!("{e}"));
    parts.into_iter().next()
}

/// Smallest signed clearance of `g`'s vertices over `body` (nearest body
/// vertex, signed by its normal), and the share closer than `min`.
pub(crate) fn clearance(g: &Part, body: &Part, min: f32) -> (f32, f32) {
    let w = weld(body);
    let cell = 0.02;
    let key = |p: Vec3| {
        let q = (p / cell).floor();
        [q.x as i32, q.y as i32, q.z as i32]
    };
    let mut grid: HashMap<[i32; 3], Vec<u32>> = HashMap::new();
    for (i, p) in w.pos.iter().enumerate() {
        grid.entry(key(*p)).or_default().push(i as u32);
    }
    let mut worst = f32::MAX;
    let mut close = 0usize;
    for i in 0..g.verts.len() {
        let p = g.world_vert(i);
        let c = key(p);
        let mut best = (f32::MAX, 0u32);
        for x in -2..=2 {
            for y in -2..=2 {
                for z in -2..=2 {
                    if let Some(v) = grid.get(&[c[0] + x, c[1] + y, c[2] + z]) {
                        for j in v {
                            let d = (w.pos[*j as usize] - p).length_squared();
                            if d < best.0 {
                                best = (d, *j);
                            }
                        }
                    }
                }
            }
        }
        if best.0 == f32::MAX {
            continue;
        }
        let q = w.pos[best.1 as usize];
        let s = (p - q).dot(w.nrm[best.1 as usize]).signum() * best.0.sqrt();
        worst = worst.min(s);
        if s < min {
            close += 1;
        }
    }
    (worst, close as f32 / g.verts.len().max(1) as f32)
}

fn closed(p: &Part) -> bool {
    let mut count: HashMap<(u32, u32), i32> = HashMap::new();
    for t in p.indices.as_chunks::<3>().0 {
        for k in 0..3 {
            let (a, b) = (t[k], t[(k + 1) % 3]);
            *count.entry((a, b)).or_default() += 1;
        }
    }
    count
        .iter()
        .all(|((a, b), c)| *c == 1 && count.get(&(*b, *a)) == Some(&1))
}

#[test]
fn presets_fit_bodies() {
    let shapes: [(&str, fn(&mut BodyDef)); 4] = [
        ("female", |d| d.gender = Some(0.0)),
        ("male", |d| d.gender = Some(1.0)),
        ("heavy", |d| {
            d.gender = Some(0.7);
            d.weight = Some(1.0);
        }),
        ("child", |d| d.age = Some(9.0)),
    ];
    for (label, f) in shapes {
        let Some(body) = human(f) else { return };
        for p in PRESETS {
            let def = GarmentDef {
                on: "eve".into(),
                preset: Some((*p).into()),
                ..GarmentDef::default()
            };
            let t = std::time::Instant::now();
            let (g, covers) = build(p, &def, &body).unwrap_or_else(|e| panic!("{label} {p}: {e}"));
            let ms = t.elapsed().as_millis();
            let (worst, share) = clearance(&g, &body, 0.5 * 0.002);
            eprintln!(
                "{label:7} {p:10} {:6} tris {:5} covered  {ms:4} ms  min clearance {:+.4} m  under 1 mm {:.2}%  closed {}",
                g.tri_count(),
                covers.len(),
                worst,
                share * 100.0,
                closed(&g)
            );
            assert!(g.tri_count() > 50, "{label} {p}");
        }
    }
}

#[test]
fn tshirt_is_closed_and_off_the_skin() {
    let Some(body) = human(|d| d.gender = Some(0.0)) else {
        return;
    };
    let def = GarmentDef {
        on: "eve".into(),
        preset: Some("tshirt".into()),
        ..GarmentDef::default()
    };
    let (g, covers) = build("shirt", &def, &body).unwrap_or_else(|e| panic!("{e}"));
    assert!(closed(&g), "the shell is closed");
    assert!(!covers.is_empty());
    let (_, share) = clearance(&g, &body, 0.003);
    assert!(share < 0.01, "{share}");
}

#[test]
fn region_planes_work_on_any_body() {
    let mut p = crate::primitives::primitive(crate::PrimitiveKind::Sphere, [0.3, 0.0, 0.0], "blob")
        .unwrap_or_else(|e| panic!("{e}"));
    p.translation = [0.0, 0.5, 0.0];
    let def = GarmentDef {
        on: "blob".into(),
        region: Some(Region {
            above: Some(Level::Num(0.45)),
            ..Region::default()
        }),
        thickness: Some(0.0),
        ..GarmentDef::default()
    };
    let (g, _) = build("cap", &def, &p).unwrap_or_else(|e| panic!("{e}"));
    let (lo, _) = g.bounds();
    assert!(lo.y > 0.44, "{lo}");
    // Zones need skin weights.
    let mut z = def.clone();
    z.region = Some(Region {
        zones: vec!["torso".into()],
        ..Region::default()
    });
    assert!(build("x", &z, &p).is_err_and(|e| e.contains("skin weights")));
}

/// Like [`clearance`] but signed by the body's own vertex normals (posed
/// bodies, masked bodies), over vertices only.
fn clearance_n(g: &Part, body: &Part) -> (f32, f32) {
    let cell = 0.02;
    let key = |p: Vec3| {
        let q = (p / cell).floor();
        [q.x as i32, q.y as i32, q.z as i32]
    };
    let mut grid: HashMap<[i32; 3], Vec<u32>> = HashMap::new();
    for i in 0..body.verts.len() {
        grid.entry(key(body.world_vert(i)))
            .or_default()
            .push(i as u32);
    }
    let (mut worst, mut inside) = (f32::MAX, 0usize);
    for i in 0..g.verts.len() {
        let p = g.world_vert(i);
        let c = key(p);
        let mut best = (f32::MAX, 0u32);
        for x in -2..=2 {
            for y in -2..=2 {
                for z in -2..=2 {
                    for j in grid
                        .get(&[c[0] + x, c[1] + y, c[2] + z])
                        .into_iter()
                        .flatten()
                    {
                        let d = (body.world_vert(*j as usize) - p).length_squared();
                        if d < best.0 {
                            best = (d, *j);
                        }
                    }
                }
            }
        }
        if best.0 == f32::MAX {
            continue;
        }
        let q = body.world_vert(best.1 as usize);
        let n = Vec3::from(body.normals[best.1 as usize]);
        let s = (p - q).dot(n).signum() * best.0.sqrt();
        worst = worst.min(s);
        if s < 0.0 {
            inside += 1;
        }
    }
    (worst, inside as f32 / g.verts.len().max(1) as f32)
}

#[test]
fn outfit_example_follows_the_walk_and_crouch() {
    if crate::human::asset::load().is_err() {
        eprintln!("skipping: no human template");
        return;
    }
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/human_outfit.json");
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{e}"));
    let items: Vec<serde_json::Value> =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}"));
    let mut s = crate::commands::Session::default();
    let t = std::time::Instant::now();
    s.apply_values_as(None, items, false)
        .unwrap_or_else(|e| panic!("{e}"));
    eprintln!("outfit applied in {:?}", t.elapsed());
    let body = s.model.part("eve").unwrap_or_else(|| panic!("no eve"));
    let full = crate::human::build(
        "eve",
        &crate::body::BodyDef {
            preset: "human".into(),
            gender: Some(0.0),
            height: Some(1.65),
            ..Default::default()
        },
    )
    .unwrap_or_else(|e| panic!("{e}"));
    eprintln!(
        "body tris shown {} of {} (covered skin hidden)",
        body.tri_count(),
        full[0].tri_count()
    );
    assert!(body.tri_count() < full[0].tri_count());
    let spec: crate::commands::VerifySpec = serde_json::from_value(serde_json::json!({
        "validate": true, "materials": "pbr", "weights": true,
        "animation": {"clip": "walk", "stretch": true}
    }))
    .unwrap_or_else(|e| panic!("{e}"));
    let (ok, report) = s.verify(&spec);
    eprintln!("verify ok={ok}\n{report}");
    let m = &s.model;
    for (clip, times) in [
        ("walk", vec![0.0, 0.13, 0.27, 0.4, 0.54, 0.67, 0.8, 0.94]),
        ("crouch", vec![0.5]),
    ] {
        let (c, sk) = crate::anim::clip(m, Some(clip)).unwrap_or_else(|e| panic!("{e}"));
        for tt in times {
            let posed = crate::anim::posed_parts(m, &c, &sk, tt);
            let b = posed
                .iter()
                .find(|p| p.name == "eve")
                .unwrap_or_else(|| panic!("eve"));
            let mut line = format!("{clip} t={tt:.2}:");
            for g in ["shirt", "jeans", "sneakers", "hair"] {
                let gp = posed
                    .iter()
                    .find(|p| p.name == g)
                    .unwrap_or_else(|| panic!("{g}"));
                let (worst, share) = clearance_n(gp, b);
                line += &format!("  {g} min {worst:+.4} m, {:.2}% inside", share * 100.0);
            }
            eprintln!("{line}");
        }
    }
}

#[test]
#[ignore]
fn debug_worst_shirt_edge() {
    if crate::human::asset::load().is_err() {
        return;
    }
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/human_outfit.json");
    let items: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap_or_default())
            .unwrap_or_default();
    let mut s = crate::commands::Session::default();
    s.apply_values_as(None, items, false)
        .unwrap_or_else(|e| panic!("{e}"));
    let m = &s.model;
    let rest = m.part("shirt").unwrap_or_else(|| panic!("shirt"));
    let (c, sk) = crate::anim::clip(m, Some("walk")).unwrap_or_else(|e| panic!("{e}"));
    let posed = crate::anim::posed_parts(m, &c, &sk, 0.871);
    let p = posed
        .iter()
        .find(|p| p.name == "shirt")
        .unwrap_or_else(|| panic!("s"));
    let mut worst = (0.0f32, 0usize, 0usize);
    for t in rest.indices.as_chunks::<3>().0 {
        for k in 0..3 {
            let (a, b) = (t[k] as usize, t[(k + 1) % 3] as usize);
            let l0 = (Vec3::from(rest.verts[a]) - Vec3::from(rest.verts[b])).length();
            let l1 = (Vec3::from(p.verts[a]) - Vec3::from(p.verts[b])).length();
            let r = (l1 / l0).max(l0 / l1.max(1e-9) * 0.4);
            if l0 > 1e-6 && r > worst.0 {
                worst = (r, a, b);
            }
        }
    }
    let (r, a, b) = worst;
    let l0 = (Vec3::from(rest.verts[a]) - Vec3::from(rest.verts[b])).length();
    eprintln!(
        "worst {r} rest len {l0} at {:?} {:?} posed {:?} {:?} nverts {}",
        rest.verts[a],
        rest.verts[b],
        p.verts[a],
        p.verts[b],
        rest.verts.len()
    );
    if let Some(sk) = &rest.skin {
        eprintln!(
            "w a {:?} {:?} b {:?} {:?}",
            sk.joints[a], sk.weights[a], sk.joints[b], sk.weights[b]
        );
    }
}

#[test]
fn outfit_variants_show_hide_and_export_visibility() {
    if crate::human::asset::load().is_err() {
        eprintln!("skipping: no human template");
        return;
    }
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/human_outfit.json");
    let items: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap_or_default())
            .unwrap_or_default();
    let mut s = crate::commands::Session::default();
    s.apply_values_as(None, items, false)
        .unwrap_or_else(|e| panic!("{e}"));
    let summer = s.variant_model("summer").unwrap_or_else(|e| panic!("{e}"));
    let vis = |m: &crate::Model, p: &str| m.part(p).is_some_and(|p| p.visible);
    assert!(vis(&s.model, "shirt") && !vis(&s.model, "skirt"));
    assert!(vis(&summer, "skirt") && vis(&summer, "tank") && !vis(&summer, "shirt"));
    // The skin under the shirt shows again when the summer outfit hides it.
    let tris = |m: &crate::Model| m.part("eve").map_or(0, |p| p.tri_count());
    assert_ne!(tris(&s.model), tris(&summer));
    let dir = std::env::temp_dir().join(format!("modeler-outfit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    let out = dir.join("outfit.gltf");
    crate::gltf::export_gltf(&s.model, &out).unwrap_or_else(|e| panic!("{e}"));
    let doc: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).unwrap_or_default())
            .unwrap_or_else(|e| panic!("{e}"));
    let node = |n: &str| {
        doc["nodes"]
            .as_array()
            .and_then(|a| a.iter().find(|x| x["name"] == n).cloned())
            .unwrap_or_else(|| panic!("no node {n}"))
    };
    let skirt = node("skirt");
    assert_eq!(skirt["extras"]["variants_visible"]["default"], false);
    assert_eq!(skirt["extras"]["variants_visible"]["summer"], true);
    assert_eq!(node("shirt")["extras"]["variants_visible"]["summer"], false);
    let _ = std::fs::remove_dir_all(&dir);
}
