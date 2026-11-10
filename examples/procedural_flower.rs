//! Builds the flower as a procedural op graph and saves it to the prefab
//! catalog, so the graph version exists alongside the baked one. No game code.
//!
//! Run: cargo run -p friendslop-modeler --example procedural_flower

use modeler::commands::Session;

fn main() {
    let mut s = Session::default();
    s.model.name = "graph-flower".into();

    // Stem, then one petal flattened in place, fanned by the radial op. The
    // source petal fills the 0 degree slot; 7 copies start a step later.
    s.op_primitive("stem", "cylinder", [0.03, 0.9, 0.0], [0.0, 0.45, 0.0], None)
        .unwrap_or_else(|e| panic!("{e}"));
    s.op_primitive("petal", "sphere", [0.16, 0.0, 0.0], [0.0, 0.95, 0.28], None)
        .unwrap_or_else(|e| panic!("{e}"));
    s.op_transform("petal", "petal", None, None, Some([0.5, 0.22, 1.0]))
        .unwrap_or_else(|e| panic!("{e}"));
    s.op_material(
        "petal",
        "petal",
        Some([0.95, 0.45, 0.65, 1.0]),
        Some(0.6),
        None,
    )
    .unwrap_or_else(|e| panic!("{e}"));
    s.op_radial_array("ring", "petal", 7, [0.0, 0.95, 0.0], 315.0, 45.0, "y")
        .unwrap_or_else(|e| panic!("{e}"));

    // Center button.
    s.op_primitive("center", "sphere", [0.1, 0.0, 0.0], [0.0, 0.95, 0.0], None)
        .unwrap_or_else(|e| panic!("{e}"));
    s.op_material(
        "center",
        "center",
        Some([0.98, 0.78, 0.18, 1.0]),
        Some(0.8),
        None,
    )
    .unwrap_or_else(|e| panic!("{e}"));

    // Leaves: primitives squashed and angled in place.
    s.op_primitive("leaf_a", "sphere", [0.1, 0.0, 0.0], [0.1, 0.45, 0.06], None)
        .unwrap_or_else(|e| panic!("{e}"));
    s.op_transform(
        "leaf_a",
        "leaf_a",
        Some([0.1, 0.45, 0.06]),
        Some([0.0, 0.866, 0.0, 0.5]),
        Some([1.0, 0.35, 1.5]),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    s.op_material("leaf_a", "leaf_a", Some([0.3, 0.65, 0.3, 1.0]), None, None)
        .unwrap_or_else(|e| panic!("{e}"));
    // The second leaf is a true mirror of the first across x = 0.
    s.op_mirror("leaf_b", "leaf_a", "x", [0.0, 0.0, 0.0])
        .unwrap_or_else(|e| panic!("{e}"));
    s.op_material("stem", "stem", Some([0.35, 0.7, 0.35, 1.0]), None, None)
        .unwrap_or_else(|e| panic!("{e}"));

    modeler::catalog::save_prefab(
        &s.model,
        "A pink flower built entirely as a procedural op graph: one stem, one \
         petal primitive (squashed and colored in place) fanned into 7 copies \
         by a radial-array op, leaves and a yellow center. Tweak any op and \
         the whole flower re-derives.",
        &["flower", "plant", "procedural", "graph"],
        "a procedural pink flower: stem + one petal + radial array ops",
    )
    .unwrap_or_else(|e| panic!("{e}"));
    println!("ops: {}", s.describe_ops());
    println!("parts: {}", s.model.parts.len());
}
