//! Performance regression guards. Each budget is several times what the
//! operation takes today on a laptop, so normal noise passes but an
//! accidental O(n^2) (bounds per pair, a whole-history clone per edit, a
//! brute-force raycast) fails loudly. Timings print with
//! `cargo test --test perf -- --nocapture`.

use std::time::{Duration, Instant};

use modeler::commands::Session;
use serde_json::{Value, json};

/// A floor, a 12 x 12 grid of boxes on it and a sphere on every box: 289
/// parts, many touching pairs.
fn scene() -> Vec<Value> {
    vec![
        json!({"type": "primitive", "name": "floor", "kind": "box",
               "size": [30, 0.2, 30], "translation": [0, 0.1, 0]}),
        json!({"type": "material", "name": "floor", "color": "#888", "flat": true}),
        json!({"type": "repeat", "var": "r", "count": 12, "items": [
            {"type": "repeat", "var": "c", "count": 12, "items": [
                {"type": "primitive", "name": "box{r}_{c}", "kind": "box", "size": 0.5,
                 "translation": ["=$r * 1.5 - 9", 0.45, "=$c * 1.5 - 9"]},
                {"type": "primitive", "name": "ball{r}_{c}", "kind": "sphere", "size": 0.25,
                 "translation": ["=$r * 1.5 - 9", 0.95, "=$c * 1.5 - 9"]}
            ]}
        ]}),
    ]
}

fn apply(s: &mut Session, items: Vec<Value>) {
    s.apply_values(None, items)
        .unwrap_or_else(|e| panic!("apply failed: {e}"));
}

/// Median of `n` runs.
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

/// The tests in this binary run on parallel threads by default; a timing
/// taken while another test saturates every core (rayon bakes, sdf meshing)
/// measured contention, not the code. Each test holds this for its run.
static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn budget(what: &str, took: Duration, max_ms: u64) {
    println!("{what}: {took:?} (budget {max_ms} ms)");
    assert!(
        took <= Duration::from_millis(max_ms),
        "{what} took {took:?}, over its {max_ms} ms budget: something got slower (profile it)"
    );
}

#[test]
fn edits_place_verify_and_autosave_stay_fast() {
    let _serial = serial();
    let dir = std::env::temp_dir().join(format!("modeler-perf-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    // SAFETY: the only test in this binary; set before anything reads it.
    unsafe { std::env::set_var("FRIENDSLOP_DATA_DIR", &dir) };

    let mut s = Session::default();
    let t0 = Instant::now();
    apply(&mut s, scene());
    budget("build 289 parts", t0.elapsed(), 1000);
    assert_eq!(s.model.parts.len(), 289);

    // A small edit on a big model: lints, summary, history record.
    // Warm up first (allocator, eval cache, first lint pass), then take the
    // median of many runs: one scheduler hiccup must not decide the result.
    let edit_at = |s: &mut Session, i: usize| {
        apply(
            s,
            vec![
                json!({"type": "primitive", "name": "probe", "kind": "box", "size": 0.05,
                        "translation": [14.0 + i as f64 * 0.01, 0.225, 14.0]}),
            ],
        );
    };
    for i in 0..3 {
        edit_at(&mut s, 100 + i);
    }
    let edit = median(15, |i| edit_at(&mut s, i));
    budget("one edit on 290 parts", edit, 50);

    // place each: 40 keys dropped onto a dense dome (a few thousand
    // triangles), so a brute-force raycast per sample would show.
    apply(
        &mut s,
        vec![
            json!({"type": "revolve", "name": "dome", "segments": 96,
                   "profile": [[0, 0.2], {"arc": {"center": [0, 0.2], "radius": 3,
                               "from": 0, "to": 90}}],
                   "translation": [0, 0, 12]}),
            json!({"type": "repeat", "var": "r", "count": 5, "items": [
                {"type": "repeat", "var": "c", "count": 8, "items": [
                    {"type": "primitive", "name": "key{r}_{c}", "kind": "sphere", "size": 0.05,
                     "translation": ["=$c * 0.6 - 2.1", 4.0, "=$r * 0.6 + 10.8"]}
                ]}
            ]}),
        ],
    );
    let place = median(3, |i| {
        apply(
            &mut s,
            vec![
                json!({"type": "place", "name": "keys_down", "parts": ["key*"],
                        "on": "dome", "each": true, "gap": i as f64 * 0.0001}),
            ],
        );
    });
    budget("place each, 40 keys on a dome", place, 800);

    // verify over every pair.
    let spec: modeler::commands::VerifySpec = serde_json::from_value(json!({
        "grounded": true, "supported": true, "collisions": true
    }))
    .unwrap_or_else(|e| panic!("{e}"));
    let t0 = Instant::now();
    let checks = s.verify_checks(&spec);
    budget("verify grounded+supported+collisions", t0.elapsed(), 800);
    for (name, ok, detail) in &checks {
        assert!(ok, "{name} failed on the perf scene: {detail}");
    }

    // Autosave: after many edits, a save appends only the new history.
    let mut st = modeler::catalog::JournalState::default();
    modeler::catalog::save_session_journaled(&mut s.model, &mut st)
        .unwrap_or_else(|e| panic!("{e}"));
    for i in 0..60 {
        apply(
            &mut s,
            vec![
                json!({"type": "primitive", "name": "probe", "kind": "box", "size": 0.05,
                        "translation": [14.0, 0.225, 14.0 - i as f64 * 0.01]}),
            ],
        );
    }
    let save = median(5, |i| {
        apply(
            &mut s,
            vec![
                json!({"type": "primitive", "name": "probe", "kind": "box", "size": 0.05,
                        "translation": [13.0, 0.225, 14.0 - i as f64 * 0.01]}),
            ],
        );
        modeler::catalog::save_session_journaled(&mut s.model, &mut st)
            .unwrap_or_else(|e| panic!("{e}"));
    });
    budget("edit + autosave with a long history", save, 250);

    let _ = std::fs::remove_dir_all(&dir);
}

/// Object-domain bakes with mesh AO on 82 parts: more parts than the old
/// fixed-size bake cache held, which rebaked everything on every edit.
#[test]
fn material_bakes_stay_cached_and_parallel() {
    let _serial = serial();
    use modeler::material::pbr::{bake_part, prebake};
    let mut s = Session::default();
    s.materials
        .ops(
            "grime",
            vec![
                json!({"type": "input", "name": "p", "of": "position"}),
                json!({"type": "noise", "name": "n", "size": 0.05, "coords": "p"}),
                json!({"type": "mesh_ao", "name": "ao"}),
                json!({"type": "math", "name": "c", "op": "mul", "a": "n", "b": "ao"}),
                json!({"type": "output", "name": "out", "base_color": "c", "ao": "ao"}),
            ],
            None,
            Some("object".into()),
            None,
            Some(256),
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
    apply(
        &mut s,
        vec![
            json!({"type": "primitive", "name": "floor", "kind": "box", "size": [10, 0.2, 10],
                   "translation": [0, 0.1, 0]}),
            json!({"type": "repeat", "var": "r", "count": 9, "items": [
                {"type": "repeat", "var": "c", "count": 9, "items": [
                    {"type": "primitive", "name": "b{r}_{c}", "kind": "box", "size": 0.5,
                     "translation": ["=$r * 1.1 - 4.5", 0.45, "=$c * 1.1 - 4.5"]}
                ]}
            ]}),
            json!({"type": "material", "name": "*", "pbr": "grime"}),
        ],
    );
    let bake_all = |s: &Session| {
        let parts: Vec<_> = s
            .model
            .parts
            .iter()
            .filter(|p| p.visible)
            .cloned()
            .collect();
        prebake(parts.iter().map(|p| &**p));
        for p in &parts {
            bake_part(p).unwrap_or_else(|e| panic!("{e}"));
        }
    };
    let t0 = Instant::now();
    bake_all(&s);
    budget(
        "cold bake, 82 object parts with mesh_ao",
        t0.elapsed(),
        2500,
    );
    let t0 = Instant::now();
    bake_all(&s);
    budget("unchanged re-bake (all cached)", t0.elapsed(), 30);
    apply(
        &mut s,
        vec![
            json!({"type": "primitive", "name": "b0_0", "kind": "box", "size": 0.5,
                    "translation": [-4.6, 0.45, -4.5]}),
        ],
    );
    let t0 = Instant::now();
    bake_all(&s);
    budget("re-bake after moving one part", t0.elapsed(), 400);
}

/// A creature the way agents build one: twelve independent sdf parts, each
/// decimated to a few hundred triangles with the dense source hidden. The
/// dense meshes are sized for their decimates and the pairs derive in
/// parallel (this took about 30 s before both).
#[test]
fn sdf_creatures_mesh_for_their_decimates() {
    let _serial = serial();
    let mut s = Session::default();
    let _ = s.new_model("critters");
    let t = Instant::now();
    apply(
        &mut s,
        vec![json!({"type": "repeat", "var": "k", "count": 12, "items": [
            {"type": "sdf", "name": "p{k}_hi", "blend": 0.03, "shapes": [
                {"shape": "ellipsoid", "size": [0.12, 0.1, 0.16], "at": ["=$k * 0.5", 0.16, 0]},
                {"shape": "sphere", "radius": 0.075, "at": ["=$k * 0.5", 0.27, 0.15]},
                {"shape": "capsule", "from": ["=$k * 0.5 + 0.07", 0.12, 0.09],
                 "to": ["=$k * 0.5 + 0.09", 0, 0.11], "radius": [0.03, 0.022]}]},
            {"type": "decimate", "name": "p{k}", "input": "p{k}_hi", "tris": 400},
            {"type": "hide_part", "part": "p{k}_hi"}
        ]})],
    );
    let took = t.elapsed();
    let dense = s
        .model
        .parts
        .iter()
        .find(|p| p.name == "p0_hi")
        .map(|p| p.tri_count())
        .unwrap_or(0);
    println!("12 sdf + decimate pairs: {took:?} (dense source {dense} tris)");
    assert!(
        dense > 0 && dense < 40_000,
        "the dense source should be meshed for a 400-triangle decimate, got {dense}"
    );
    budget("12 sdf + decimate pairs", took, 6000);
    // The cache derived in parallel matches a plain serial re-derivation.
    let spec: modeler::commands::VerifySpec =
        serde_json::from_value(json!({"rebuild": true})).unwrap_or_else(|e| panic!("{e}"));
    let (ok, v) = s.verify(&spec);
    assert!(ok, "{v}");
}

/// A batch evaluates its graph items once, yet a failing item is still
/// the one the error names, and nothing is applied.
#[test]
fn a_deferred_batch_names_its_failing_item() {
    let _serial = serial();
    let mut s = Session::default();
    let _ = s.new_model("defer");
    let e = s
        .apply_values(
            None,
            vec![
                json!({"type": "primitive", "name": "a", "kind": "box", "size": 1}),
                json!({"type": "primitive", "name": "b", "kind": "box", "size": 1,
                       "translation": [5, 0, 0]}),
                json!({"type": "boolean", "name": "c", "op": "intersection", "a": "a", "b": "b"}),
            ],
        )
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    assert!(e.contains("#2"), "{e}");
    assert!(s.model.ops.is_empty(), "the batch rolled back");
}

/// `verify collisions` on ~200 parts whose boxes crowd each other: rounded
/// slabs and columns in a dense block standing on a terrain that every part
/// sinks into (the terrain's box covers them all), like a building on a
/// hillside. Every box-overlapping pair used to run an exact intersection.
#[test]
fn collisions_on_200_crowded_parts_stay_fast() {
    let _serial = serial();
    let dir = std::env::temp_dir().join(format!("modeler-perf-coll-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    // SAFETY: set before anything in this test reads it; same dir per process.
    unsafe { std::env::set_var("FRIENDSLOP_DATA_DIR", &dir) };
    let mut s = Session::default();
    apply(
        &mut s,
        vec![
            json!({"type": "terrain", "name": "ground", "size": [40, 40], "resolution": 0.25,
                   "height": 1.5, "seed": 3}),
            json!({"type": "repeat", "var": "r", "count": 10, "items": [
                {"type": "repeat", "var": "c", "count": 10, "items": [
                    {"type": "primitive", "name": "slab{r}_{c}", "kind": "box",
                     "size": [1.2, 0.3, 1.2], "round": 0.02,
                     "translation": ["=$r * 1.0 - 5", 2.0, "=$c * 1.0 - 5"]},
                    {"type": "revolve", "name": "col{r}_{c}", "segments": 32,
                     "profile": [[0, 0], [0.12, 0], [0.1, 2.2], [0, 2.2]],
                     "translation": ["=$r * 1.0 - 4.6", -0.2, "=$c * 1.0 - 4.6"]}
                ]}
            ]}),
        ],
    );
    assert!(s.model.parts.len() >= 200);
    let spec: modeler::commands::VerifySpec =
        serde_json::from_value(json!({"collisions": true})).unwrap_or_else(|e| panic!("{e}"));
    let t0 = Instant::now();
    let checks = s.verify_checks(&spec);
    let took = t0.elapsed();
    assert!(checks.iter().any(|(n, _, _)| n == "collisions"));
    budget("verify collisions, 201 crowded parts", took, 3000);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A default `body` biped (skin of ~30 oval joints, meshed at full detail)
/// derives quickly; with `tris` it is meshed only as finely as it needs.
#[test]
fn a_default_body_biped_builds_fast() {
    let _serial = serial();
    let mut s = Session::default();
    let _ = s.new_model("person");
    let t = Instant::now();
    apply(
        &mut s,
        vec![json!({"type": "body", "name": "person", "preset": "biped"})],
    );
    budget("body biped (full detail)", t.elapsed(), BIPED_MS);
    let t = Instant::now();
    apply(
        &mut s,
        vec![
            json!({"type": "body", "name": "light", "preset": "biped", "height": 1.6, "tris": 4000}),
        ],
    );
    budget("body biped, tris 4000", t.elapsed(), BIPED_MS);
}

const BIPED_MS: u64 = 6000;

/// A building with furniture: ~320 boxes and cylinders, floor slabs, walls,
/// and rows of tables / chairs / pillars that touch or sink into each other.
fn building() -> Vec<Value> {
    vec![
        json!({"type": "primitive", "name": "slab", "kind": "box", "round": 0.01,
               "size": [24, 0.3, 24], "translation": [0, 0.15, 0]}),
        json!({"type": "primitive", "name": "wall_n", "kind": "box", "round": 0.01,
               "size": [24, 3, 0.3], "translation": [0, 1.8, -12]}),
        json!({"type": "primitive", "name": "wall_s", "kind": "box", "round": 0.01,
               "size": [24, 3, 0.3], "translation": [0, 1.8, 12]}),
        json!({"type": "primitive", "name": "wall_e", "kind": "box", "round": 0.01,
               "size": [0.3, 3, 24], "translation": [12, 1.8, 0]}),
        json!({"type": "primitive", "name": "wall_w", "kind": "box", "round": 0.01,
               "size": [0.3, 3, 24], "translation": [-12, 1.8, 0]}),
        json!({"type": "repeat", "var": "r", "count": 8, "items": [
            {"type": "repeat", "var": "c", "count": 8, "items": [
                {"type": "primitive", "name": "table{r}_{c}", "kind": "box", "round": 0.01,
                 "size": [1.0, 0.05, 0.6],
                 "translation": ["=$r * 2.6 - 9", 0.75, "=$c * 2.6 - 9"]},
                {"type": "primitive", "name": "leg{r}_{c}", "kind": "cylinder",
                 "size": [0.04, 0.5], "segments": 16,
                 "translation": ["=$r * 2.6 - 9", 0.45, "=$c * 2.6 - 9"]},
                {"type": "primitive", "name": "seat{r}_{c}", "kind": "box", "round": 0.01,
                 "size": [0.4, 0.05, 0.4],
                 "translation": ["=$r * 2.6 - 9", 0.45, "=$c * 2.6 - 8.3"]},
                {"type": "primitive", "name": "back{r}_{c}", "kind": "box", "round": 0.01,
                 "size": [0.4, 0.4, 0.04],
                 "translation": ["=$r * 2.6 - 9", 0.7, "=$c * 2.6 - 8.1"]},
                {"type": "primitive", "name": "lamp{r}_{c}", "kind": "cylinder",
                 "size": [0.1, 0.2], "segments": 24,
                 "translation": ["=$r * 2.6 - 9.2", 0.9, "=$c * 2.6 - 9"], "round": 0.01}
            ]}
        ]}),
    ]
}

#[test]
fn building_of_325_boxes_verifies_and_applies_fast() {
    let _serial = serial();
    let dir = std::env::temp_dir().join(format!("modeler-perf-bld-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    // SAFETY: set before anything in this test reads it; same dir per process.
    unsafe { std::env::set_var("FRIENDSLOP_DATA_DIR", &dir) };
    let mut s = Session::default();
    let t0 = Instant::now();
    s.apply_values_as(None, building(), false)
        .unwrap_or_else(|e| panic!("apply failed: {e}"));
    budget(
        "apply 325-part building (brief summary)",
        t0.elapsed(),
        8000,
    );
    assert!(s.model.parts.len() >= 320);
    for (label, spec) in [
        ("collisions", json!({"collisions": true})),
        ("grounded", json!({"grounded": true})),
        ("validate", json!({"validate": true})),
        (
            "grounded+supported+collisions+validate",
            json!({
            "grounded": true, "supported": true, "collisions": true, "validate": true}),
        ),
    ] {
        let spec: modeler::commands::VerifySpec =
            serde_json::from_value(spec).unwrap_or_else(|e| panic!("{e}"));
        let t0 = Instant::now();
        let _ = s.verify_checks(&spec);
        budget(&format!("verify {label}, 325 parts"), t0.elapsed(), 6000);
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// 300 rounded tiles and cylinders edge to edge on a slab: nearly every
/// part touches 4-8 others, so contact (not just overlap) work dominates.
#[test]
fn tiled_floor_of_300_touching_parts_verifies_fast() {
    let _serial = serial();
    let dir = std::env::temp_dir().join(format!("modeler-perf-tile-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    // SAFETY: set before anything in this test reads it; same dir per process.
    unsafe { std::env::set_var("FRIENDSLOP_DATA_DIR", &dir) };
    let mut s = Session::default();
    s.apply_values_as(
        None,
        vec![
            json!({"type": "primitive", "name": "slab", "kind": "box", "size": [20, 0.2, 16],
                   "translation": [0, 0.1, 0]}),
            json!({"type": "repeat", "var": "r", "count": 20, "items": [
                {"type": "repeat", "var": "c", "count": 15, "items": [
                    {"type": "primitive", "name": "tile{r}_{c}", "kind": "box", "round": 0.01,
                     "size": [1.0, 0.1, 1.0],
                     "translation": ["=$r * 1.0 - 9.5", 0.25, "=$c * 1.0 - 7"]}
                ]}
            ]}),
        ],
        false,
    )
    .unwrap_or_else(|e| panic!("apply failed: {e}"));
    let spec: modeler::commands::VerifySpec = serde_json::from_value(json!({
        "grounded": true, "supported": true, "collisions": true
    }))
    .unwrap_or_else(|e| panic!("{e}"));
    let t0 = Instant::now();
    let checks = s.verify_checks(&spec);
    budget(
        "verify grounded+supported+collisions, 301 touching",
        t0.elapsed(),
        6000,
    );
    for (name, ok, detail) in &checks {
        assert!(ok, "{name} failed: {detail}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
