//! Named model parameters. An `apply_ops` value may be `"$tower_h"` (the
//! parameter's value) or an expression `"=$tower_h / 2 + 0.1"` (`+ - * /`,
//! parentheses, numbers, `$names`; a leading `$` also starts an expression).
//! The model keeps each op's unresolved item (its template), so changing a
//! parameter re-resolves and re-derives every op that reads it.

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub type Params = BTreeMap<String, f64>;

/// True when `s` is a parameter reference or expression.
fn is_expr(s: &str) -> bool {
    s.starts_with('$') || s.starts_with('=')
}

/// `v` with every reference/expression replaced by its number.
pub fn resolve(v: &Value, params: &Params) -> Result<Value, String> {
    Ok(match v {
        Value::String(s) if is_expr(s) => {
            let x = eval(s.trim_start_matches('='), params)?;
            serde_json::Number::from_f64(x)
                .map(Value::Number)
                .ok_or_else(|| format!("`{s}` is not a finite number ({x})"))?
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|x| resolve(x, params))
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, x)| resolve(x, params).map(|r| (k.clone(), r)))
                .collect::<Result<_, _>>()?,
        ),
        other => other.clone(),
    })
}

/// Parameter names referenced anywhere in `v`.
pub fn refs(v: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    collect(v, &mut out);
    out
}

fn collect(v: &Value, out: &mut BTreeSet<String>) {
    match v {
        Value::String(s) if is_expr(s) => {
            let b = s.as_bytes();
            let mut i = 0;
            while i < b.len() {
                if b[i] == b'$' {
                    let start = i + 1;
                    let mut j = start;
                    while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                        j += 1;
                    }
                    out.insert(s[start..j].to_string());
                    i = j;
                } else {
                    i += 1;
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|x| collect(x, out)),
        Value::Object(map) => map.values().for_each(|x| collect(x, out)),
        _ => {}
    }
}

/// Evaluates an arithmetic expression over `params`.
pub fn eval(src: &str, params: &Params) -> Result<f64, String> {
    let mut p = Parser {
        s: src.as_bytes(),
        i: 0,
        params,
        src,
    };
    let v = p.expr()?;
    p.ws();
    if p.i != p.s.len() {
        return Err(format!("unexpected `{}` in `{src}`", &src[p.i..]));
    }
    Ok(v)
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    params: &'a Params,
    src: &'a str,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn peek(&mut self) -> Option<u8> {
        self.ws();
        self.s.get(self.i).copied()
    }
    fn expr(&mut self) -> Result<f64, String> {
        let mut v = self.term()?;
        while let Some(c @ (b'+' | b'-')) = self.peek() {
            self.i += 1;
            let r = self.term()?;
            v = if c == b'+' { v + r } else { v - r };
        }
        Ok(v)
    }
    fn term(&mut self) -> Result<f64, String> {
        let mut v = self.factor()?;
        while let Some(c @ (b'*' | b'/')) = self.peek() {
            self.i += 1;
            let r = self.factor()?;
            v = if c == b'*' { v * r } else { v / r };
        }
        Ok(v)
    }
    fn factor(&mut self) -> Result<f64, String> {
        match self.peek() {
            Some(b'-') => {
                self.i += 1;
                Ok(-self.factor()?)
            }
            Some(b'(') => {
                self.i += 1;
                let v = self.expr()?;
                if self.peek() != Some(b')') {
                    return Err(format!("missing `)` in `{}`", self.src));
                }
                self.i += 1;
                Ok(v)
            }
            Some(b'$') => {
                self.i += 1;
                let start = self.i;
                while self.i < self.s.len()
                    && (self.s[self.i].is_ascii_alphanumeric() || self.s[self.i] == b'_')
                {
                    self.i += 1;
                }
                let name = &self.src[start..self.i];
                self.params.get(name).copied().ok_or_else(|| {
                    let known: Vec<&str> = self.params.keys().map(String::as_str).collect();
                    format!(
                        "unknown parameter `${name}` (known: {})",
                        if known.is_empty() {
                            "none - set them with apply_ops params".to_string()
                        } else {
                            known.join(", ")
                        }
                    )
                })
            }
            Some(c) if c.is_ascii_digit() || c == b'.' => {
                let start = self.i;
                while self.i < self.s.len()
                    && (self.s[self.i].is_ascii_digit()
                        || self.s[self.i] == b'.'
                        || self.s[self.i] == b'e')
                {
                    self.i += 1;
                }
                self.src[start..self.i]
                    .parse()
                    .map_err(|_| format!("bad number in `{}`", self.src))
            }
            _ => Err(format!("expected a number or `$param` in `{}`", self.src)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resolves_refs_and_expressions() {
        let p: Params = [("h".to_string(), 2.0), ("w".to_string(), 0.5)].into();
        assert_eq!(eval("$h / 2 + 0.1", &p), Ok(1.1));
        assert_eq!(eval("-($h - $w) * 2", &p), Ok(-3.0));
        let v = json!({"size": [0.8, "$h", 0], "translation": [0, "=$h/2", 0], "name": "t"});
        assert_eq!(
            resolve(&v, &p),
            Ok(json!({"size": [0.8, 2.0, 0], "translation": [0, 1.0, 0], "name": "t"}))
        );
        assert_eq!(
            refs(&v),
            ["h".to_string()].into_iter().collect::<BTreeSet<_>>()
        );
        assert!(eval("$nope", &p).is_err_and(|e| e.contains("unknown parameter")));
    }
}
