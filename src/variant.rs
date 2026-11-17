//! Cosmetic variants ("skins"): alternate looks for one model (red / blue
//! team, clean / rusty), switchable in game. A `variant` item names a look:
//! material overrides on parts (names or `*` globs) and an optional style
//! token patch. Geometry never changes; the base model is the variant
//! named [`DEFAULT`].
//!
//! A variant is applied after the whole graph (like `move`): the parts are
//! derived as usual (re-resolved under the patched style when it has one),
//! then each matched part's material is overridden in place, so a variant
//! changes exactly the parts it names and nothing a copy or a cut made from
//! them.

use crate::material::PbrRef;
use crate::material::finish::Finish;
use crate::{Material, Model, Op};
use serde::{Deserialize, Serialize};

/// The base look's variant name.
pub const DEFAULT: &str = "default";

/// One material override of a variant: the parts it covers and the same
/// fields as a `material` item.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Look {
    /// A part name or `*` glob.
    pub parts: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_color: Option<[f32; 4]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roughness: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metalness: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pbr: Option<PbrRef>,
    /// A plain colour on purpose: drops the base look's pbr material.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub flat: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish: Option<Finish>,
}

impl Look {
    /// The base material with this look over it (material item semantics:
    /// a pbr resets the factors to 1, given fields win; `flat` drops pbr).
    pub fn over(&self, base: &Material) -> Material {
        let mut m = base.clone();
        if let Some(f) = &self.finish {
            m.finish = (!f.is_empty()).then(|| f.clone());
        }
        if let Some(r) = &self.pbr {
            m.base_color = [1.0; 4];
            m.roughness = 1.0;
            m.metalness = 1.0;
            m.pbr = Some(r.clone());
            m.flat = false;
        } else if self.flat {
            m.pbr = None;
            m.flat = true;
            // A flat colour over a pbr part: plain factors unless given.
            if base.pbr.is_some() {
                let d = Material::default();
                m.roughness = d.roughness;
                m.metalness = d.metalness;
            }
        }
        if let Some(c) = self.base_color {
            m.base_color = c;
        }
        if let Some(r) = self.roughness {
            m.roughness = r;
        }
        if let Some(x) = self.metalness {
            m.metalness = x;
        }
        m
    }

    /// Every pbr reference of the look (to embed graphs).
    pub fn pbr_mut(&mut self) -> Option<&mut PbrRef> {
        self.pbr.as_mut()
    }
}

/// The model's variant names in op order (without [`DEFAULT`]).
pub fn names(model: &Model) -> Vec<String> {
    model
        .ops
        .iter()
        .filter_map(|o| match o {
            Op::Variant { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect()
}

/// The variant op named `name`.
pub fn find<'a>(
    model: &'a Model,
    name: &str,
) -> Option<(&'a [Look], Option<&'a serde_json::Value>)> {
    model.ops.iter().find_map(|o| match o {
        Op::Variant {
            name: n,
            materials,
            style,
        } if n == name => Some((materials.as_slice(), style.as_ref())),
        _ => None,
    })
}

/// "unknown variant" with the list of known ones.
pub fn unknown(model: &Model, name: &str) -> String {
    let mut known = vec![DEFAULT.to_string()];
    known.extend(names(model));
    format!(
        "no variant `{name}` (variants: {}); add one with a `variant` item",
        known.join(", ")
    )
}

/// Overrides the materials of the parts each look matches. Glob looks run
/// first, then exact names, so `{"body*": .., "body_trim": ..}` gives the
/// trim its own look whatever order the keys came in. Returns the part
/// names changed.
pub fn apply_looks(model: &mut Model, looks: &[Look]) -> Vec<String> {
    let mut changed = Vec::new();
    let ordered = looks
        .iter()
        .filter(|l| l.parts.contains('*'))
        .chain(looks.iter().filter(|l| !l.parts.contains('*')));
    for look in ordered {
        for part in &mut model.parts {
            if !crate::name_matches(&look.parts, &part.name) {
                continue;
            }
            let m = look.over(&part.material);
            std::sync::Arc::make_mut(part).material = m;
            if !changed.contains(&part.name) {
                changed.push(part.name.clone());
            }
        }
    }
    changed
}

/// Looks that match no part of the model (named, for a warning).
pub fn unmatched(model: &Model) -> Vec<String> {
    let mut out = Vec::new();
    for o in &model.ops {
        let Op::Variant {
            name, materials, ..
        } = o
        else {
            continue;
        };
        for l in materials {
            if !model
                .parts
                .iter()
                .any(|p| crate::name_matches(&l.parts, &p.name))
            {
                out.push(format!("variant `{name}`: `{}` matches no part", l.parts));
            }
        }
    }
    out
}

/// The model as it looks in variant `name` (history-less): [`DEFAULT`] is
/// the model itself; a style patch re-derives every item that reads tokens
/// (through `restyle`, which a [`crate::commands::Session`] provides with
/// its working materials), then the looks override the matched parts.
pub fn derive_with(
    model: &Model,
    name: &str,
    restyle: impl FnOnce(&Model, crate::style::Style) -> Result<Model, String>,
) -> Result<Model, String> {
    let mut m = model.clone_without_history();
    m.active_variant = None;
    if name == DEFAULT {
        return Ok(m);
    }
    let Some((looks, style)) = find(model, name) else {
        return Err(unknown(model, name));
    };
    let looks = looks.to_vec();
    if let Some(patch) = style {
        let mut st = m.style.clone();
        st.merge(patch)
            .map_err(|e| format!("variant `{name}` style: {e}"))?;
        m = restyle(&m, st).map_err(|e| format!("variant `{name}` style: {e}"))?;
        m.active_variant = None;
    }
    apply_looks(&mut m, &looks);
    Ok(m)
}

/// [`derive_with`] restyling in a bare session (library materials only):
/// for prefab instances and code without a session.
pub fn derive(model: &Model, name: &str) -> Result<Model, String> {
    derive_with(model, name, |m, st| {
        let mut s = crate::commands::Session::default();
        s.model = m.clone();
        s.set_style(st).map_err(|e| e.to_string())?;
        Ok(s.model.clone_without_history())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::Session;

    fn session() -> Session {
        let mut s = Session::default();
        let items = serde_json::json!([
            {"type": "primitive", "name": "body", "kind": "box", "size": [1, 1, 1], "translation": [0, 0.5, 0]},
            {"type": "primitive", "name": "body_trim", "kind": "box", "size": [1.1, 0.1, 1.1], "translation": [0, 0.05, 0]},
            {"type": "primitive", "name": "flag", "kind": "box", "size": [0.1, 0.5, 0.1], "translation": [0, 1.25, 0]},
            {"type": "material", "name": "body", "color": "#888888", "flat": true},
            {"type": "material", "name": "body_trim", "color": "#888888", "flat": true},
            {"type": "material", "name": "flag", "color": "#888888", "flat": true},
            {"type": "variant", "name": "red", "materials": {
                "body*": {"color": "#cc3333", "flat": true},
                "body_trim": {"color": "#222222", "roughness": 0.2, "flat": true}}},
        ]);
        s.apply_values_as(None, items.as_array().cloned().unwrap_or_default(), false)
            .unwrap_or_else(|e| panic!("{e}"));
        s
    }

    #[test]
    fn a_variant_overrides_only_the_parts_it_matches() {
        let s = session();
        assert_eq!(names(&s.model), vec!["red".to_string()]);
        let base = s.variant_model(DEFAULT).unwrap_or_else(|e| panic!("{e}"));
        let red = s.variant_model("red").unwrap_or_else(|e| panic!("{e}"));
        let col = |m: &Model, p: &str| m.part(p).map(|p| p.material.base_color);
        // Geometry is the same, materials differ only where matched.
        assert_eq!(base.parts.len(), red.parts.len());
        assert_eq!(col(&red, "flag"), col(&base, "flag"));
        assert_ne!(col(&red, "body"), col(&base, "body"));
        // The exact name beats the glob.
        let trim = red.part("body_trim").map(|p| p.material.clone());
        assert!(trim.is_some_and(|m| (m.roughness - 0.2).abs() < 1e-6 && m.base_color[0] < 0.2));
        // The live model is untouched.
        assert_eq!(col(&s.model, "body"), col(&base, "body"));
        assert!(s.variant_model("blue").is_err());
    }

    #[test]
    fn materials_verify_checks_every_variant() {
        let mut s = session();
        let spec: crate::commands::VerifySpec =
            serde_json::from_value(serde_json::json!({"materials": true}))
                .unwrap_or_else(|e| panic!("{e}"));
        assert!(s.verify(&spec).0, "{}", s.verify(&spec).1);
        // A variant that sends a part back to the default grey fails.
        let item = crate::spec::parse_item(
            0,
            &serde_json::json!({"type": "variant", "name": "grey", "materials": {
                "flag": {"color": [0.7, 0.7, 0.7], "roughness": 0.6, "metalness": 0}}}),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        s.apply_batch(vec![item]).unwrap_or_else(|e| panic!("{e}"));
        let (ok, text) = s.verify(&spec);
        assert!(!ok && text.contains("variant grey"), "{text}");
        let only: crate::commands::VerifySpec =
            serde_json::from_value(serde_json::json!({"materials": true, "variant": "red"}))
                .unwrap_or_else(|e| panic!("{e}"));
        assert!(s.verify(&only).0, "{}", s.verify(&only).1);
    }

    #[test]
    fn a_style_patch_re_resolves_token_items() {
        let mut s = Session::default();
        s.set_style(
            serde_json::from_value(serde_json::json!({"colors": {"primary": "#3333cc"}}))
                .unwrap_or_else(|e| panic!("{e}")),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let items = serde_json::json!([
            {"type": "primitive", "name": "body", "kind": "box", "size": 1},
            {"type": "material", "name": "body", "color": "@primary", "flat": true},
            {"type": "variant", "name": "red", "style": {"colors": {"primary": "#cc3333"}}},
        ]);
        s.apply_values_as(None, items.as_array().cloned().unwrap_or_default(), false)
            .unwrap_or_else(|e| panic!("{e}"));
        let red = s.variant_model("red").unwrap_or_else(|e| panic!("{e}"));
        let c = |m: &Model| m.part("body").map_or([0.0; 4], |p| p.material.base_color);
        assert!(c(&red)[0] > c(&red)[2], "{:?}", c(&red));
        assert!(c(&s.model)[2] > c(&s.model)[0]);
    }

    #[test]
    fn export_writes_khr_materials_variants_and_extras() {
        let s = session();
        let (doc, _bin, report) = crate::gltf::build_variants(
            &s.model,
            None,
            None,
            &crate::export_opt::Options::game(),
            None,
            false,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(report.contains("variants: default, red"), "{report}");
        let names: Vec<&str> = doc["extensions"]["KHR_materials_variants"]["variants"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v["name"].as_str())
            .collect();
        assert_eq!(names, ["default", "red"]);
        assert!(
            doc["extensionsUsed"]
                .as_array()
                .is_some_and(|a| a.iter().any(|e| e == "KHR_materials_variants"))
        );
        let node = |n: &str| {
            doc["nodes"]
                .as_array()
                .and_then(|a| a.iter().find(|x| x["name"] == n).cloned())
                .unwrap_or_default()
        };
        let mappings = |n: &str| {
            let m = node(n)["mesh"].as_u64().unwrap_or(0) as usize;
            doc["meshes"][m]["primitives"][0]["extensions"]["KHR_materials_variants"]["mappings"]
                .clone()
        };
        // body changes look: two mappings, red's material differs from base.
        let body = node("body");
        let (d, r) = (
            body["extras"]["variants"]["default"].as_u64(),
            body["extras"]["variants"]["red"].as_u64(),
        );
        assert!(d.is_some() && r.is_some() && d != r, "{body}");
        assert_eq!(mappings("body").as_array().map(Vec::len), Some(2));
        let m = node("body")["mesh"].as_u64().unwrap_or(0) as usize;
        assert_eq!(doc["meshes"][m]["primitives"][0]["material"].as_u64(), d);
        // flag is untouched: one mapping for both variants.
        let flag = node("flag");
        assert_eq!(
            flag["extras"]["variants"]["default"],
            flag["extras"]["variants"]["red"]
        );
        assert_eq!(mappings("flag")[0]["variants"], serde_json::json!([0, 1]));
        // The red body material is really red.
        let red =
            &doc["materials"][r.unwrap_or(0) as usize]["pbrMetallicRoughness"]["baseColorFactor"];
        assert!(red[0].as_f64() > red[2].as_f64(), "{red}");
    }
}
