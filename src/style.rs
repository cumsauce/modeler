//! Style tokens: a model's design system. A palette of named colours, named
//! material presets, numeric feel tokens and per-op-type field defaults,
//! which `apply_ops` items reference instead of literal values:
//!
//! - `"@primary"` (or `"@primary.darken(0.2)"`, `.lighten(x)`, `.mix(@accent, x)`,
//!   `.alpha(x)`) anywhere a string value is: a colour token,
//! - `"$bevel"` / `"=$bevel * 2"`: a number token (style numbers are params;
//!   a model param of the same name wins),
//! - `"style": "trim"` on a `material` item: a material preset (explicit
//!   fields win),
//! - `defaults`: fields filled into every item of a type when omitted
//!   (`{"revolve": {"segments": "$segments"}}`).
//!
//! Items that read the style keep their unresolved form (a template), so
//! changing a token or loading another style re-derives them.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Style {
    /// The style's name (the library entry it came from, if any).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The feel in words, for what tokens can't say ("chunky low-poly toy,
    /// soft rounded edges, warm pastels").
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub brief: String,
    /// Named colours, any form a colour field accepts (stored as given).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub colors: BTreeMap<String, Value>,
    /// Named material presets: fields of a `material` item.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub materials: BTreeMap<String, Map<String, Value>>,
    /// Numeric feel tokens (bevel, wall, segments, ...), read as `$name`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub numbers: BTreeMap<String, f64>,
    /// Per item type, fields filled in when an item omits them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub defaults: BTreeMap<String, Map<String, Value>>,
}

impl crate::Doc for Style {
    fn version(&self) -> u64 {
        0
    }
}

impl Style {
    pub fn is_empty(&self) -> bool {
        self.brief.is_empty()
            && self.colors.is_empty()
            && self.materials.is_empty()
            && self.numbers.is_empty()
            && self.defaults.is_empty()
    }

    /// Merges `patch` over this style (a `null` value removes that token).
    pub fn merge(&mut self, patch: &Value) -> Result<(), String> {
        let Value::Object(p) = patch else {
            return Err("style tokens must be an object".into());
        };
        for (k, v) in p {
            match k.as_str() {
                "name" | "brief" => {
                    let s = v.as_str().unwrap_or_default().to_string();
                    if k == "name" {
                        self.name = s;
                    } else {
                        self.brief = s;
                    }
                }
                "colors" | "materials" | "numbers" | "defaults" => {
                    let Value::Object(m) = v else {
                        return Err(format!("{k}: must be an object of name -> value"));
                    };
                    for (name, x) in m {
                        let remove = x.is_null();
                        match k.as_str() {
                            "colors" => {
                                if remove {
                                    self.colors.remove(name);
                                } else {
                                    self.colors.insert(name.clone(), x.clone());
                                }
                            }
                            "numbers" => {
                                if remove {
                                    self.numbers.remove(name);
                                } else {
                                    let n = x.as_f64().ok_or_else(|| {
                                        format!("numbers.{name}: must be a number, got {x}")
                                    })?;
                                    self.numbers.insert(name.clone(), n);
                                }
                            }
                            _ => {
                                let target = if k == "materials" {
                                    &mut self.materials
                                } else {
                                    &mut self.defaults
                                };
                                if remove {
                                    target.remove(name);
                                } else {
                                    let Value::Object(f) = x else {
                                        return Err(format!(
                                            "{k}.{name}: must be an object of fields"
                                        ));
                                    };
                                    target.insert(name.clone(), f.clone());
                                }
                            }
                        }
                    }
                }
                other => {
                    return Err(format!(
                        "unknown style field `{other}` (brief, colors, materials, numbers, defaults)"
                    ));
                }
            }
        }
        self.check()
    }

    /// Every colour token parses and every modifier reference resolves.
    pub fn check(&self) -> Result<(), String> {
        for name in self.colors.keys() {
            self.color(name, 0)
                .map_err(|e| format!("colors.{name}: {e}"))?;
        }
        for (name, m) in &self.materials {
            if m.contains_key("name") || m.contains_key("input") || m.contains_key("type") {
                return Err(format!(
                    "materials.{name}: a preset holds look fields only (color, roughness, metalness, pbr, params, scale)"
                ));
            }
            expand_value(&Value::Object(m.clone()), self)
                .map_err(|e| format!("materials.{name}: {e}"))?;
        }
        Ok(())
    }

    /// A colour token as linear-free sRGB 0..1 RGBA (the value as given).
    fn color(&self, name: &str, depth: usize) -> Result<[f32; 4], String> {
        if depth > 8 {
            return Err(format!(
                "@{name}: colour tokens refer to each other in a loop"
            ));
        }
        let v = self.colors.get(name).ok_or_else(|| {
            let known: Vec<&str> = self.colors.keys().map(String::as_str).collect();
            format!(
                "no colour token `@{name}` (palette: {})",
                if known.is_empty() {
                    "empty".into()
                } else {
                    known.join(", ")
                }
            )
        })?;
        match v {
            Value::String(s) if s.starts_with('@') => self.token(s, depth + 1),
            other => crate::spec::parse_color_value(other),
        }
    }

    /// `@name` or `@name.mod(args).mod(args)`.
    fn token(&self, s: &str, depth: usize) -> Result<[f32; 4], String> {
        let body = &s[1..];
        let (name, mut rest) = match body.find('.') {
            Some(i) => (&body[..i], &body[i..]),
            None => (body, ""),
        };
        let mut c = self.color(name, depth)?;
        while let Some(r) = rest.strip_prefix('.') {
            let open = r
                .find('(')
                .ok_or_else(|| format!("{s}: expected .modifier(args)"))?;
            let close = matching_paren(r, open).ok_or_else(|| format!("{s}: unclosed `(`"))?;
            let m = &r[..open];
            let args: Vec<&str> = split_args(&r[open + 1..close]);
            let num = |i: usize| -> Result<f32, String> {
                args.get(i)
                    .and_then(|a| a.trim().parse::<f32>().ok())
                    .ok_or_else(|| format!("{s}: .{m} needs a number argument"))
            };
            c = match m {
                "darken" => mix(c, [0.0, 0.0, 0.0, c[3]], num(0)?),
                "lighten" => mix(c, [1.0, 1.0, 1.0, c[3]], num(0)?),
                "alpha" => [c[0], c[1], c[2], num(0)?],
                "mix" => {
                    let other = args.first().map(|a| a.trim()).unwrap_or_default();
                    let o = if other.starts_with('@') {
                        self.token(other, depth + 1)?
                    } else {
                        crate::spec::parse_color_value(&Value::String(other.to_string()))?
                    };
                    mix(c, o, num(1)?)
                }
                "saturate" | "desaturate" => {
                    let t = num(0)? * if m == "desaturate" { -1.0 } else { 1.0 };
                    let l = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
                    let f = |x: f32| (l + (x - l) * (1.0 + t)).clamp(0.0, 1.0);
                    [f(c[0]), f(c[1]), f(c[2]), c[3]]
                }
                other => {
                    return Err(format!(
                        "{s}: unknown modifier .{other} (darken, lighten, mix, alpha, saturate, desaturate)"
                    ));
                }
            };
            rest = &r[close + 1..];
        }
        Ok(c)
    }

    /// A colour token resolved to `#rrggbbaa`.
    pub fn resolve_color(&self, s: &str) -> Result<String, String> {
        let c = self.token(s, 0)?;
        let b = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
        Ok(format!(
            "#{:02x}{:02x}{:02x}{:02x}",
            b(c[0]),
            b(c[1]),
            b(c[2]),
            b(c[3])
        ))
    }

    /// Params for resolving items: the style numbers under the model's own.
    pub fn params_under(&self, model: &crate::params::Params) -> crate::params::Params {
        let mut p: crate::params::Params = self.numbers.clone();
        p.extend(model.iter().map(|(k, v)| (k.clone(), *v)));
        p
    }

    /// Text listing (style show).
    pub fn describe(&self) -> String {
        if self.is_empty() {
            return "no style set (style set / style load)".into();
        }
        let mut s = format!(
            "style{}\n",
            if self.name.is_empty() {
                String::new()
            } else {
                format!(" `{}`", self.name)
            }
        );
        if !self.brief.is_empty() {
            s += &format!("brief: {}\n", self.brief);
        }
        if !self.colors.is_empty() {
            s += "colors (@name):\n";
            for (k, v) in &self.colors {
                let hex = self.resolve_color(&format!("@{k}")).unwrap_or_default();
                s += &format!(
                    "  @{k} = {}{}\n",
                    v,
                    if v.as_str() == Some(&hex) {
                        String::new()
                    } else {
                        format!(" ({hex})")
                    }
                );
            }
        }
        if !self.materials.is_empty() {
            s += "materials (material item \"style\": name):\n";
            for (k, v) in &self.materials {
                s += &format!("  {k}: {}\n", Value::Object(v.clone()));
            }
        }
        if !self.numbers.is_empty() {
            s += "numbers ($name):\n";
            for (k, v) in &self.numbers {
                s += &format!("  ${k} = {v}\n");
            }
        }
        if !self.defaults.is_empty() {
            s += "defaults (filled into items that omit them):\n";
            for (k, v) in &self.defaults {
                s += &format!("  {k}: {}\n", Value::Object(v.clone()));
            }
        }
        s
    }

    /// A swatch sheet: palette chips, then material presets, labelled.
    pub fn swatches(&self) -> Result<Vec<u8>, String> {
        use crate::raster::Canvas;
        let chip = 88usize;
        let gap = 12usize;
        let cols = 6usize;
        let mut cells: Vec<(String, [f32; 4], Option<String>)> = Vec::new();
        for k in self.colors.keys() {
            cells.push((format!("@{k}"), self.token(&format!("@{k}"), 0)?, None));
        }
        for (k, m) in &self.materials {
            let m = match expand_value(&Value::Object(m.clone()), self)? {
                Value::Object(m) => m,
                _ => Map::new(),
            };
            let c = m
                .get("color")
                .or_else(|| m.get("base_color"))
                .map(crate::spec::parse_color_value)
                .transpose()?
                .unwrap_or([0.8, 0.8, 0.8, 1.0]);
            let mut sub = Vec::new();
            if let Some(r) = m.get("roughness") {
                sub.push(format!("r{r}"));
            }
            if let Some(r) = m.get("metalness") {
                sub.push(format!("m{r}"));
            }
            if let Some(Value::String(p)) = m.get("pbr") {
                sub.push(p.clone());
            }
            cells.push((k.clone(), c, Some(sub.join(" "))));
        }
        if cells.is_empty() {
            return Err("the style has no colours or materials to show".into());
        }
        let rows = cells.len().div_ceil(cols);
        let w = gap + cols.min(cells.len()) * (chip + gap);
        let h = gap + rows * (chip + 40 + gap);
        let mut c = Canvas::new(w, h, [32, 33, 36, 255]);
        for (i, (label, col, sub)) in cells.iter().enumerate() {
            let x0 = gap + (i % cols) * (chip + gap);
            let y0 = gap + (i / cols) * (chip + 40 + gap);
            let b = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
            let rgba = [b(col[0]), b(col[1]), b(col[2]), 255];
            let preset = sub.is_some();
            for y in 0..chip {
                for x in 0..chip {
                    // Presets are discs with a highlight, palette chips squares.
                    let (dx, dy) = (x as f32 - chip as f32 / 2.0, y as f32 - chip as f32 / 2.0);
                    let r = (dx * dx + dy * dy).sqrt() / (chip as f32 / 2.0);
                    if preset && r > 1.0 {
                        continue;
                    }
                    let mut p = rgba;
                    if preset {
                        let shade = 1.15 - 0.45 * ((dx + dy) / chip as f32 + 0.5).clamp(0.0, 1.0);
                        for ch in p.iter_mut().take(3) {
                            *ch = ((*ch as f32) * shade).min(255.0) as u8;
                        }
                    }
                    c.set((x0 + x) as i64, (y0 + y) as i64, p);
                }
            }
            c.text(
                x0,
                y0 + chip + 6,
                &label.chars().take(14).collect::<String>(),
                1,
            );
            let hex = format!("#{:02x}{:02x}{:02x}", rgba[0], rgba[1], rgba[2]);
            let sub = sub.clone().filter(|s| !s.is_empty()).unwrap_or(hex);
            c.text(
                x0,
                y0 + chip + 20,
                &sub.chars().take(14).collect::<String>(),
                1,
            );
        }
        c.to_png()
    }
}

fn mix(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    let t = t.clamp(0.0, 1.0);
    [0, 1, 2, 3].map(|i| a[i] + (b[i] - a[i]) * t)
}

fn matching_paren(s: &str, open: usize) -> Option<usize> {
    let mut depth = 0;
    for (i, ch) in s.char_indices().skip(open) {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn split_args(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut start) = (0, 0);
    for (i, ch) in s.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

/// Replaces every `"@token..."` string inside `v` with its `#rrggbbaa`.
fn expand_value(v: &Value, style: &Style) -> Result<Value, String> {
    Ok(match v {
        Value::String(s) if s.starts_with('@') => Value::String(style.resolve_color(s)?),
        Value::Array(a) => Value::Array(
            a.iter()
                .map(|x| expand_value(x, style))
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, x)| expand_value(x, style).map(|r| (k.clone(), r)))
                .collect::<Result<_, _>>()?,
        ),
        other => other.clone(),
    })
}

/// True when `raw` reads the style (a colour token, a preset, or a field
/// the style's defaults fill in).
pub fn reads_style(raw: &Value, style: &Style) -> bool {
    fn has_at(v: &Value) -> bool {
        match v {
            Value::String(s) => s.starts_with('@'),
            Value::Array(a) => a.iter().any(has_at),
            Value::Object(m) => m.values().any(has_at),
            _ => false,
        }
    }
    if has_at(raw) {
        return true;
    }
    let Value::Object(m) = raw else {
        return false;
    };
    if m.contains_key("style") {
        return true;
    }
    let ty = m.get("type").and_then(Value::as_str).unwrap_or_default();
    style
        .defaults
        .get(ty)
        .is_some_and(|d| d.keys().any(|k| !m.contains_key(k)))
}

/// An item with the style applied: defaults filled in, a material preset
/// merged under the item's own fields, and colour tokens resolved. `$`
/// number tokens are left for params resolution.
pub fn expand(raw: &Value, style: &Style) -> Result<Value, String> {
    let mut v = raw.clone();
    if let Value::Object(m) = &mut v {
        let ty = m
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        // A prefab's `style` is its own mode ("scene" / a token patch for
        // the prefab), resolved against the prefab, not this scene.
        if ty == "prefab" {
            return Ok(v);
        }
        // A terrain's `style` is its shape (hills / ridges / dunes / ...).
        if ty == "terrain" && m.get("style").is_some_and(|s| s.is_string()) {
            return Ok(v);
        }
        if let Some(name) = m.remove("style") {
            let name = name
                .as_str()
                .ok_or("style: must be a material preset name")?;
            if ty != "material" {
                return Err(format!(
                    "style: only material items take a preset (this is `{ty}`)"
                ));
            }
            let preset = style.materials.get(name).ok_or_else(|| {
                let known: Vec<&str> = style.materials.keys().map(String::as_str).collect();
                format!(
                    "style: no material preset `{name}` (presets: {})",
                    if known.is_empty() {
                        "none".into()
                    } else {
                        known.join(", ")
                    }
                )
            })?;
            for (k, x) in preset {
                // `base_color` and `color` are one field.
                let alias = match k.as_str() {
                    "color" => Some("base_color"),
                    "base_color" => Some("color"),
                    _ => None,
                };
                if !m.contains_key(k) && !alias.is_some_and(|a| m.contains_key(a)) {
                    m.insert(k.clone(), x.clone());
                }
            }
        }
        if let Some(d) = style.defaults.get(&ty) {
            for (k, x) in d {
                if !m.contains_key(k) {
                    m.insert(k.clone(), x.clone());
                }
            }
        }
    }
    expand_value(&v, style)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_terrain_keeps_its_own_style_under_a_scene_style() {
        let item = json!({"type": "terrain", "name": "g", "style": "flat"});
        assert_eq!(expand(&item, &toy()), Ok(item));
    }

    fn toy() -> Style {
        let mut s = Style::default();
        s.merge(&json!({
            "brief": "toy",
            "colors": {"primary": "#ff0000", "ink": "#000000", "alias": "@primary"},
            "materials": {"plastic": {"color": "@primary", "roughness": 0.4}},
            "numbers": {"bevel": 0.02},
            "defaults": {"revolve": {"segments": 12}}
        }))
        .unwrap_or_else(|e| panic!("{e}"));
        s
    }

    #[test]
    fn colour_tokens_and_modifiers_resolve() {
        let s = toy();
        assert_eq!(
            s.resolve_color("@primary").ok().as_deref(),
            Some("#ff0000ff")
        );
        assert_eq!(s.resolve_color("@alias").ok().as_deref(), Some("#ff0000ff"));
        assert_eq!(
            s.resolve_color("@primary.darken(0.5)").ok().as_deref(),
            Some("#800000ff")
        );
        assert_eq!(
            s.resolve_color("@primary.mix(@ink, 1)").ok().as_deref(),
            Some("#000000ff")
        );
        assert_eq!(
            s.resolve_color("@primary.alpha(0)").ok().as_deref(),
            Some("#ff000000")
        );
        let e = s.resolve_color("@nope").err().unwrap_or_default();
        assert!(e.contains("palette: alias, ink, primary"), "{e}");
    }

    #[test]
    fn presets_and_defaults_fill_only_missing_fields() {
        let s = toy();
        let m = expand(
            &json!({"type": "material", "name": "a", "style": "plastic", "roughness": 0.9}),
            &s,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(m["color"], "#ff0000ff");
        assert_eq!(m["roughness"], 0.9);
        assert!(m.get("style").is_none());
        let r = expand(&json!({"type": "revolve", "name": "v", "segments": 64}), &s)
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(r["segments"], 64);
        let r =
            expand(&json!({"type": "revolve", "name": "v"}), &s).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(r["segments"], 12);
        assert!(reads_style(&json!({"type": "revolve", "name": "v"}), &s));
        assert!(!reads_style(
            &json!({"type": "revolve", "name": "v", "segments": 3}),
            &s
        ));
    }

    #[test]
    fn bad_tokens_are_rejected_at_set() {
        let mut s = toy();
        assert!(s.merge(&json!({"colors": {"x": "@missing"}})).is_err());
        assert!(s.merge(&json!({"colours": {}})).is_err());
    }

    #[test]
    fn changing_a_token_restyles_the_model_in_one_undo() {
        use crate::commands::Session;
        let mut sess = Session::default();
        sess.set_style(toy()).unwrap_or_else(|e| panic!("{e}"));
        let items = json!([
            {"type": "primitive", "name": "a", "kind": "box", "size": "=$bevel * 10", "translation": [0, 0.1, 0]},
            {"type": "primitive", "name": "b", "kind": "box", "size": 0.2, "translation": [1, 0.1, 0]},
            {"type": "material", "name": "a", "style": "plastic"},
            {"type": "material", "name": "b", "color": "@ink"}
        ]);
        sess.apply_values_as(None, items.as_array().cloned().unwrap_or_default(), false)
            .unwrap_or_else(|e| panic!("{e}"));
        let col = |s: &Session, n: &str| {
            s.model
                .parts
                .iter()
                .find(|p| p.name == n)
                .map(|p| p.material.base_color)
                .unwrap_or_default()
        };
        let red = col(&sess, "a");
        assert!(red[0] > 0.9 && red[1] < 0.1, "{red:?}");
        let mut st = sess.model.style.clone();
        st.merge(&json!({"colors": {"primary": "#0000ff"}, "numbers": {"bevel": 0.03}}))
            .unwrap_or_else(|e| panic!("{e}"));
        let msg = sess.set_style(st).unwrap_or_else(|e| panic!("{e}"));
        assert!(msg.contains("re-derived 2 op(s)"), "{msg}");
        let blue = col(&sess, "a");
        assert!(blue[2] > 0.9 && blue[0] < 0.1, "{blue:?}");
        let ink = col(&sess, "b");
        assert!(ink[0] < 0.01, "{ink:?}");
        sess.undo().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(col(&sess, "a"), red);
        assert_eq!(sess.model.style.colors["primary"], "#ff0000");
    }
}
