//! Repro: small real-scale silhouettes (gun parts, a few cm) used to SIGSEGV the server.
#![allow(clippy::unwrap_used)]
use serde_json::json;

#[test]
fn small_silhouettes_build() {
    let items = vec![
        json!({"type":"silhouette","name":"receiver","side":[[-0.17,0.935],[0.12,0.935],[0.12,1.03],[-0.17,1.03]],"top":[[-0.025,-0.17],[0.025,-0.17],[0.025,0.12],[-0.025,0.12]],"round":0.003}),
        json!({"type":"silhouette","name":"rear_sight_block","side":[[0.1,1.0],[0.17,1.0],[0.17,1.055],[0.1,1.06]],"top":[[-0.018,0.1],[0.018,0.1],[0.018,0.17],[-0.018,0.17]],"round":0.002}),
        json!({"type":"silhouette","name":"gas_block","side":[[0.345,0.985],[0.375,0.985],[0.375,1.05],[0.355,1.05]],"top":[[-0.015,0.345],[0.015,0.345],[0.015,0.375],[-0.015,0.375]],"round":0.003}),
        json!({"type":"silhouette","name":"front_sight_base","side":[[0.405,0.985],[0.435,0.985],[0.435,1.03],[0.405,1.03]],"top":[[-0.014,0.405],[0.014,0.405],[0.014,0.435],[-0.014,0.435]],"round":0.003}),
        json!({"type":"silhouette","name":"stock","side":[[-0.17,0.95],[-0.17,1.025],[-0.44,1.0],[-0.445,0.86],[-0.41,0.86],[-0.25,0.925]],"top":[[-0.02,-0.17],[0.02,-0.17],[0.022,-0.44],[-0.022,-0.44]],"round":0.006}),
        json!({"type":"silhouette","name":"pistol_grip","side":[[-0.07,0.935],[-0.105,0.935],[-0.15,0.82],[-0.125,0.81],[-0.095,0.82]],"top":[[-0.016,-0.16],[0.016,-0.16],[0.016,-0.06],[-0.016,-0.06]],"round":0.008}),
    ];
    for it in items {
        let name = it["name"].as_str().unwrap_or("?").to_string();
        eprintln!("building {name}");
        let mut s = modeler::commands::Session::default();
        s.apply_values(None, vec![it])
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(s.model.part(&name).is_some(), "{name}: no part");
    }
}
