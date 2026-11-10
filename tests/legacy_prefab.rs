//! Prefabs written before the typed op enums (string kind/op/axis) must load
//! unchanged, and bad values must be rejected at deserialization.

use modeler::{BoolOp, Model, Op, PrimitiveKind};

const LEGACY: &str = r#"{
  "name": "legacy", "parts": [], "version": 1,
  "ops": [
    {"primitive": {"name": "a", "kind": "box", "size": [2,2,2], "translation": [0,0,0]}},
    {"primitive": {"name": "b", "kind": "cylinder", "size": [0.5,3,0], "translation": [0,0,0]}},
    {"boolean": {"name": "c", "op": "difference", "a": "a", "b": "b"}},
    {"radial_array": {"name": "r", "input": "a", "count": 3, "axis": "z"}},
    {"mirror": {"name": "m", "input": "a", "axis": "x"}}
  ]
}"#;

#[test]
fn legacy_string_ops_load_into_enums() {
    let mut model: Model = serde_json::from_str(LEGACY).unwrap_or_else(|e| panic!("{e}"));
    assert!(matches!(
        model.ops[0],
        Op::Primitive {
            kind: PrimitiveKind::Box,
            ..
        }
    ));
    assert!(matches!(
        model.ops[2],
        Op::Boolean {
            op: BoolOp::Difference,
            ..
        }
    ));
    // The wire format is unchanged on the way back out.
    let json = serde_json::to_string(&model.ops[2]).unwrap_or_else(|e| panic!("{e}"));
    assert!(json.contains(r#""op":"difference""#), "{json}");
    model.refresh().unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn bad_enum_values_fail_to_deserialize() {
    let bad = LEGACY.replace(r#""kind": "box""#, r#""kind": "dodecahedron""#);
    assert!(serde_json::from_str::<Model>(&bad).is_err());
}

#[test]
fn format_version_gates_future_files_but_not_legacy_ones() {
    let legacy: Model = serde_json::from_str(LEGACY).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(legacy.format_version, 0, "old files have no version field");
    assert!(legacy.check_format().is_ok());

    let fresh = Model::named("fresh");
    assert_eq!(fresh.format_version, modeler::FORMAT_VERSION);
    let json = serde_json::to_string(&fresh).unwrap_or_else(|e| panic!("{e}"));
    assert!(json.contains("\"format_version\""));

    let future = LEGACY.replace(
        "\"version\": 1,",
        "\"version\": 1, \"format_version\": 999,",
    );
    let model: Model = serde_json::from_str(&future).unwrap_or_else(|e| panic!("{e}"));
    assert!(model.check_format().is_err());
}
