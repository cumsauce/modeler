//! Validates the glTF of a dressed, rigged, animated human (the `eve`
//! fixture of `tests/common/character.rs`): the spec's own validation via
//! the `gltf` crate (no external validator is installed), then the
//! contracts `modeler_bevy` relies on, read from the raw JSON and buffers.
//!
//! Skips cleanly without `data/human/human.bin`.
#![allow(
    clippy::unwrap_used,
    clippy::chunks_exact_to_as_chunks,
    clippy::useless_conversion
)]

mod common;

use common::character;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;

struct Gl {
    doc: Value,
    bin: Vec<u8>,
    dir: std::path::PathBuf,
}

impl Gl {
    fn load(path: &Path) -> Gl {
        let doc: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let uri = doc["buffers"][0]["uri"].as_str().unwrap();
        let bin = std::fs::read(path.parent().unwrap().join(uri)).unwrap();
        Gl {
            doc,
            bin,
            dir: path.parent().unwrap().to_path_buf(),
        }
    }

    fn view(&self, i: u64) -> &[u8] {
        let v = &self.doc["bufferViews"][i as usize];
        let off = v["byteOffset"].as_u64().unwrap_or(0) as usize;
        let len = v["byteLength"].as_u64().unwrap() as usize;
        &self.bin[off..off + len]
    }

    fn view_offset(&self, i: u64) -> u64 {
        self.doc["bufferViews"][i as usize]["byteOffset"]
            .as_u64()
            .unwrap_or(0)
    }

    fn f32s(&self, view: u64) -> Vec<f32> {
        self.view(view)
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    }

    fn nodes(&self) -> &Vec<Value> {
        self.doc["nodes"].as_array().unwrap()
    }
}

fn export(tag: &str) -> Option<(Gl, std::path::PathBuf, String)> {
    if !character::human_available() {
        eprintln!("skipped: no data/human/human.bin");
        return None;
    }
    let (_, path, report) =
        character::export(tag, &json!({"preset": "game", "lods": [0.5, 0.2]})).unwrap();
    Some((Gl::load(&path), path, report))
}

#[test]
fn gltf_passes_spec_validation() {
    let Some((_, path, _)) = export("validate") else {
        return;
    };
    // `import` parses and validates the document, loads buffers and images
    // (our KTX2 textures are not decodable by the crate's image loader, so
    // validate the JSON with the crate's own validator instead).
    let text = std::fs::read(&path).unwrap();
    let doc = gltf::Gltf::from_slice(&text).expect("parses and validates minimally");
    let root = doc.document.into_json();
    use gltf::json::validation::Validate;
    let mut errors = Vec::new();
    root.validate(&root, gltf::json::Path::new, &mut |p, e| {
        errors.push(format!("{}: {e:?}", p()))
    });
    assert!(errors.is_empty(), "{errors:#?}");
}

/// Reads a (possibly sparse) VEC3 accessor into dense positions.
fn read_vec3(g: &Gl, acc: usize) -> Vec<[f32; 3]> {
    let a = &g.doc["accessors"][acc];
    let n = a["count"].as_u64().unwrap() as usize;
    let mut out = vec![[0.0f32; 3]; n];
    if let Some(v) = a["bufferView"].as_u64() {
        for (o, c) in out.iter_mut().zip(g.f32s(v).chunks_exact(3)) {
            *o = [c[0], c[1], c[2]];
        }
    }
    if let Some(s) = a.get("sparse") {
        let cnt = s["count"].as_u64().unwrap() as usize;
        let ib = g.view(s["indices"]["bufferView"].as_u64().unwrap());
        let ty = s["indices"]["componentType"].as_u64().unwrap();
        let vals = g.f32s(s["values"]["bufferView"].as_u64().unwrap());
        for k in 0..cnt {
            let i = match ty {
                5125 => u32::from_le_bytes(ib[k * 4..k * 4 + 4].try_into().unwrap()) as usize,
                5123 => u16::from_le_bytes(ib[k * 2..k * 2 + 2].try_into().unwrap()) as usize,
                _ => u8::from(ib[k]) as usize,
            };
            out[i] = [vals[k * 3], vals[k * 3 + 1], vals[k * 3 + 2]];
        }
    }
    out
}

#[test]
fn morph_targets_are_well_formed_on_every_primitive() {
    let Some((g, _, _)) = export("morph") else {
        return;
    };
    let mut sparse = 0;
    let mut meshes_with_targets = 0;
    for (mi, mesh) in g.doc["meshes"].as_array().unwrap().iter().enumerate() {
        for prim in mesh["primitives"].as_array().unwrap() {
            let Some(targets) = prim["targets"].as_array() else {
                continue;
            };
            meshes_with_targets += 1;
            let names = mesh["extras"]["targetNames"].as_array().unwrap();
            let weights = mesh["weights"].as_array().unwrap();
            assert_eq!(targets.len(), weights.len(), "mesh {mi} weights");
            assert_eq!(targets.len(), names.len(), "mesh {mi} targetNames");
            let n = g.doc["accessors"][prim["attributes"]["POSITION"].as_u64().unwrap() as usize]
                ["count"]
                .as_u64()
                .unwrap();
            for t in targets {
                let acc = t["POSITION"].as_u64().unwrap() as usize;
                let a = &g.doc["accessors"][acc];
                assert_eq!(
                    a["count"].as_u64().unwrap(),
                    n,
                    "target count = vertex count"
                );
                if let Some(s) = a.get("sparse") {
                    sparse += 1;
                    let cnt = s["count"].as_u64().unwrap();
                    assert!(cnt > 0 && cnt <= n);
                    let iv = s["indices"]["bufferView"].as_u64().unwrap();
                    assert_eq!(g.view_offset(iv) % 4, 0, "u32 indices are 4-aligned");
                    let ib = g.view(iv);
                    assert_eq!(ib.len() as u64, cnt * 4, "index view = count u32");
                    let idx: Vec<u32> = ib
                        .chunks_exact(4)
                        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                        .collect();
                    assert!(
                        idx.windows(2).all(|w| w[0] < w[1]),
                        "indices strictly sorted"
                    );
                    assert!((*idx.last().unwrap() as u64) < n);
                    let vv = s["values"]["bufferView"].as_u64().unwrap();
                    assert_eq!(
                        g.view(vv).len() as u64,
                        cnt * 12,
                        "values view = count vec3"
                    );
                    assert_eq!(g.view_offset(vv) % 4, 0);
                    // min / max cover the values (zeros elsewhere count too).
                    let (lo, hi) = (&a["min"], &a["max"]);
                    for v in read_vec3(&g, acc) {
                        for c in 0..3 {
                            assert!(v[c] as f64 >= lo[c].as_f64().unwrap() - 1e-6);
                            assert!(v[c] as f64 <= hi[c].as_f64().unwrap() + 1e-6);
                        }
                    }
                }
            }
        }
    }
    assert!(meshes_with_targets >= 3, "body + its LODs carry targets");
    assert!(sparse > 0, "correctives are sparse");
    // Every LOD node's mesh carries the same targets as its full part.
    let by_name: HashMap<&str, &Value> = g
        .nodes()
        .iter()
        .filter_map(|n| Some((n["name"].as_str()?, n)))
        .collect();
    let mut lods = 0;
    for (name, node) in &by_name {
        let Some(base) = name.split(".lod").next().filter(|b| b != name) else {
            continue;
        };
        lods += 1;
        let m = |n: &Value| n["mesh"].as_u64().map(|i| &g.doc["meshes"][i as usize]);
        let (Some(lm), Some(bm)) = (m(node), by_name.get(base).and_then(|b| m(b))) else {
            continue;
        };
        assert_eq!(
            lm["extras"]["targetNames"], bm["extras"]["targetNames"],
            "{name} keeps the target order"
        );
    }
    assert!(lods >= 2, "lod nodes exist");
}

#[test]
fn correctives_reference_real_targets_and_joints() {
    let Some((g, _, _)) = export("corr") else {
        return;
    };
    let joints: Vec<String> = g.doc["skins"][0]["joints"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| {
            g.nodes()[j.as_u64().unwrap() as usize]["name"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    let mut seen = 0;
    for node in g.nodes() {
        let c = &node["extras"]["correctives"];
        let Some(list) = c.as_array() else { continue };
        let mesh = &g.doc["meshes"][node["mesh"].as_u64().unwrap() as usize];
        let names = mesh["extras"]["targetNames"].as_array().unwrap();
        assert_eq!(
            &mesh["extras"]["correctives"], c,
            "node and mesh tables agree"
        );
        for d in list {
            seen += 1;
            let t = d["target"].as_u64().unwrap() as usize;
            assert!(t < names.len());
            assert_eq!(names[t], d["name"], "driver names its slot");
            assert!(joints.contains(&d["bone"].as_str().unwrap().to_string()));
            assert!(d["axis"].is_array() && d["degrees"].is_number());
        }
    }
    assert!(seen > 20, "{seen} correctives");
}

#[test]
fn animations_carry_weights_root_motion_and_events() {
    let Some((g, _, _)) = export("anim") else {
        return;
    };
    let anims = g.doc["animations"].as_array().unwrap();
    let names: Vec<&str> = anims.iter().filter_map(|a| a["name"].as_str()).collect();
    for n in ["walk", "talk", "step"] {
        assert!(names.contains(&n), "{names:?}");
    }
    let mut weight_channels = 0;
    for a in anims {
        for ch in a["channels"].as_array().unwrap() {
            if ch["target"]["path"] != "weights" {
                continue;
            }
            weight_channels += 1;
            let s = &a["samplers"][ch["sampler"].as_u64().unwrap() as usize];
            let frames = g.doc["accessors"][s["input"].as_u64().unwrap() as usize]["count"]
                .as_u64()
                .unwrap();
            let out = g.doc["accessors"][s["output"].as_u64().unwrap() as usize]["count"]
                .as_u64()
                .unwrap();
            let node = &g.nodes()[ch["target"]["node"].as_u64().unwrap() as usize];
            let targets = g.doc["meshes"][node["mesh"].as_u64().unwrap() as usize]["weights"]
                .as_array()
                .unwrap()
                .len() as u64;
            assert_eq!(
                out,
                frames * targets,
                "{} weights = keys x targets",
                a["name"]
            );
        }
    }
    assert!(weight_channels >= 1);
    // talk keys the smile; walk has no morphs.
    let step = anims.iter().find(|a| a["name"] == "step").unwrap();
    let rm = &step["extras"]["root_motion"];
    assert_eq!(rm["axes"], "xz");
    let samples = rm["samples"].as_array().unwrap();
    assert!(samples.len() > 10);
    assert_eq!(samples[0][0], 0.0);
    let last = samples.last().unwrap();
    assert!((last[3].as_f64().unwrap() - 1.2).abs() < 0.05, "{last}");
    assert!((rm["delta"][2].as_f64().unwrap() - 1.2).abs() < 0.05);
    let ev = step["extras"]["events"].as_array().unwrap();
    assert_eq!(ev.len(), 2);
    assert_eq!(ev[1]["name"], "footstep_r");
    assert_eq!(ev[1]["data"]["volume"], 0.8);
    assert_eq!(step["extras"]["loop"], true);
    // The hip's travel is removed from the exported tracks.
    let hip = g.nodes().iter().position(|n| n["name"] == "hip").unwrap();
    for ch in step["channels"].as_array().unwrap() {
        if ch["target"]["node"] == hip && ch["target"]["path"] == "translation" {
            let s = &step["samplers"][ch["sampler"].as_u64().unwrap() as usize];
            let v = g.f32s(
                g.doc["accessors"][s["output"].as_u64().unwrap() as usize]["bufferView"]
                    .as_u64()
                    .unwrap(),
            );
            let z: Vec<f32> = v.chunks_exact(3).map(|c| c[2]).collect();
            let span = z.iter().cloned().fold(f32::MIN, f32::max)
                - z.iter().cloned().fold(f32::MAX, f32::min);
            assert!(span < 0.3, "travel left in the hip: {span}");
        }
    }
}

#[test]
fn skins_variants_sway_limits_and_textures() {
    let Some((g, _, report)) = export("misc") else {
        return;
    };
    // LOD nodes share the part's skin.
    let by_name: HashMap<&str, &Value> = g
        .nodes()
        .iter()
        .filter_map(|n| Some((n["name"].as_str()?, n)))
        .collect();
    let body = by_name["eve"];
    assert!(body["skin"].is_number());
    for lod in ["eve.lod1", "eve.lod2"] {
        assert_eq!(by_name[lod]["skin"], body["skin"], "{lod} shares the skin");
        assert!(by_name[lod]["extras"]["lod"]["level"].is_number());
    }
    let l0 = &body["extras"]["lod"];
    assert!(
        l0["level"] == 0 && l0["near"] == 0.0 && l0["far"].is_number(),
        "{l0}"
    );
    assert!(by_name["eve.lod2"]["extras"]["lod"]["far"].is_null());
    assert_eq!(g.doc["skins"].as_array().unwrap().len(), 1);
    // Variants.
    assert!(
        g.doc["extensionsUsed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e == "KHR_materials_variants")
    );
    let vs = g.doc["extensions"]["KHR_materials_variants"]["variants"]
        .as_array()
        .unwrap();
    assert_eq!(vs[0]["name"], "default");
    assert_eq!(vs[1]["name"], "bare");
    assert_eq!(vs[2]["name"], "red");
    let red_mat = by_name["shirt"]["extras"]["variants"]["red"]
        .as_u64()
        .unwrap();
    assert_ne!(
        Some(red_mat),
        by_name["shirt"]["extras"]["variants"]["default"].as_u64()
    );
    let shirt = by_name["shirt"];
    assert!(
        shirt["extras"]["variants"]["default"].is_number(),
        "{}",
        shirt["extras"]
    );
    assert_eq!(
        shirt["extras"]["variants_visible"]["bare"], false,
        "{}",
        shirt["extras"]
    );
    assert_eq!(shirt["extras"]["variants_visible"]["default"], true);
    // Sway, clip table and limits on the rig.
    let rig = by_name["eve_rig"];
    assert!(
        rig["extras"]["sway"]["bones"].as_array().unwrap().len() > 3,
        "{report}"
    );
    assert!(
        !rig["extras"]["sway"]["kind"].is_null(),
        "{}",
        rig["extras"]["sway"]
    );
    let table = rig["extras"]["clips"].as_object().unwrap();
    for n in ["walk", "talk", "step"] {
        assert!(table.contains_key(n), "clip table lists {n}");
    }
    assert_eq!(table["step"]["events"].as_array().map(|e| e.len()), Some(2));
    assert!(
        g.nodes().iter().any(|n| n["extras"]["limits"].is_object()),
        "joint limits exported"
    );
    // Every texture uri resolves.
    let images = g.doc["images"].as_array().unwrap();
    assert!(!images.is_empty());
    for im in images {
        let uri = im["uri"].as_str().unwrap();
        assert!(g.dir.join(uri).exists(), "{uri}");
        let head = std::fs::read(g.dir.join(uri)).unwrap();
        assert_eq!(&head[1..7], b"KTX 20", "{uri} is KTX2");
    }
}
