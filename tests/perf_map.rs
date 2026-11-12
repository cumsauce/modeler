//! A map-sized perf case: a 220 m terrain at 1 m cells, a dozen prefab
//! instances whose sources have real undo histories, and markers. Maps are
//! where the window and the server got slow, so this times what they do on
//! every call: an edit, markers, a document switch, rays, verify.
//! `cargo test --test perf_map -- --nocapture` prints the timings.

use std::time::{Duration, Instant};

use modeler::commands::Session;
use serde_json::{Value, json};

fn apply(s: &mut Session, items: Vec<Value>) {
    s.apply_values(None, items)
        .unwrap_or_else(|e| panic!("apply failed: {e}"));
}

fn median(n: usize, mut f: impl FnMut(usize)) -> Duration {
    let mut t: Vec<Duration> = (0..n)
        .map(|i| {
            let t0 = Instant::now();
            f(i);
            t0.elapsed()
        })
        .collect();
    t.sort();
    t[n / 2]
}

fn budget(what: &str, took: Duration, max_ms: u64) {
    println!("{what}: {took:?} (budget {max_ms} ms)");
    assert!(
        took <= Duration::from_millis(max_ms),
        "{what} took {took:?}, over its {max_ms} ms budget"
    );
}

#[test]
fn a_map_stays_fast() {
    let dir = std::env::temp_dir().join(format!("modeler-perf-map-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    // SAFETY: the only test in this binary; set before anything reads it.
    unsafe { std::env::set_var("FRIENDSLOP_DATA_DIR", &dir) };

    let mut s = Session::default();
    // Two prefabs, each with 40 edits of history.
    for name in ["hut", "crate"] {
        s.new_model(name).unwrap_or_else(|e| panic!("{e}"));
        for i in 0..40 {
            apply(
                &mut s,
                vec![
                    json!({"type": "primitive", "name": "body", "kind": "box",
                           "size": [2.0 + i as f64 * 0.01, 2.0, 2.0], "translation": [0, 1, 0]}),
                    json!({"type": "primitive", "name": "roof", "kind": "cone",
                           "size": [1.6, 1.0], "translation": [0, 2.5, 0]}),
                    json!({"type": "marker", "name": "door", "kind": "spot",
                           "translation": [0, 0, 1.1]}),
                ],
            );
        }
        s.save_prefab_checked("perf", &[], "", true)
            .unwrap_or_else(|e| panic!("{e}"));
    }

    s.new_model("map").unwrap_or_else(|e| panic!("{e}"));
    let mut items = vec![
        json!({"type": "terrain", "name": "ground", "size": [220, 220],
                               "resolution": 1, "height": 2.4, "scale": 32, "seed": 11}),
    ];
    for i in 0..12 {
        items.push(json!({"type": "prefab", "name": format!("p{i}"),
                          "prefab": if i % 2 == 0 { "hut" } else { "crate" },
                          "translation": [i as f64 * 8.0 - 48.0, 0, 20]}));
    }
    for i in 0..30 {
        items.push(
            json!({"type": "marker", "name": format!("m{i}"), "kind": "spot",
                          "translation": [i as f64 * 3.0 - 45.0, 0, -10], "size": [2, 1, 2]}),
        );
    }
    let t0 = Instant::now();
    apply(&mut s, items);
    budget("build the map", t0.elapsed(), 1000);

    let edit = median(5, |i| {
        apply(
            &mut s,
            vec![json!({"type": "marker", "name": "m0", "kind": "spot",
                        "translation": [i as f64, 0, -10]})],
        );
    });
    budget("one marker edit on the map", edit, 50);

    let markers = median(5, |_| {
        let m = modeler::game::markers(&s.model).unwrap_or_else(|e| panic!("{e}"));
        assert!(m.len() >= 42);
    });
    budget("markers() with prefab instances", markers, 10);

    // Two agents alternating: switch away and back.
    let switch = median(5, |_| {
        s.scene("open", Some("hut"))
            .unwrap_or_else(|e| panic!("{e}"));
        s.scene("open", Some("map"))
            .unwrap_or_else(|e| panic!("{e}"));
    });
    budget("switch away from the map and back", switch, 300);

    let rays = median(3, |_| {
        for k in 0..20 {
            let hits = modeler::contact::ray_cast(
                &s.model,
                bevy::math::Vec3::new(k as f32 * 5.0 - 50.0, 100.0, 3.0),
                bevy::math::Vec3::NEG_Y,
                None,
            );
            assert!(!hits.is_empty());
        }
    });
    budget("20 rays down onto the map", rays, 30);

    let spec: modeler::commands::VerifySpec =
        serde_json::from_value(json!({"grounded": true})).unwrap_or_else(|e| panic!("{e}"));
    let t0 = Instant::now();
    let _ = s.verify_checks(&spec);
    budget("verify grounded on the map", t0.elapsed(), 3000);
    // Solids are cached across calls: an agent verifies after every edit.
    let t0 = Instant::now();
    let _ = s.verify_checks(&spec);
    budget("verify grounded again", t0.elapsed(), 150);

    let _ = std::fs::remove_dir_all(&dir);
}
