//! `apply_ops { ops_file }`: a batch's items read from a JSON file in the
//! caller's project, for generated batches too big to pass inline (hundreds
//! of boxes, a generated tri_mesh). The file is read once and its items are
//! applied like inline ones, so history records the items themselves and the
//! document never depends on the file again.

use serde_json::Value;
use std::path::{Path, PathBuf};

/// Where `file` is, inside `project` (relative paths are taken from it).
/// Refuses paths that leave the project, through `..` or a symlink, and calls
/// with no project (the shared library has no folder to read from).
/// With no project (an agent working outside a game folder), an absolute
/// path is read as given: the caller wrote the file itself, and the items
/// are only data, applied like inline ones.
pub(crate) fn resolve(project: Option<&Path>, file: &str) -> Result<PathBuf, String> {
    if project.is_none() {
        let p = Path::new(file);
        if !p.is_absolute() {
            return Err(format!(
                "ops_file `{file}`: this call has no project, so give an absolute path (relative paths are taken from the caller's game project)"
            ));
        }
        return p
            .canonicalize()
            .map_err(|e| format!("ops_file `{file}`: {e}"));
    }
    resolve_as(project, file, "ops_file")
}

/// [`resolve`] for any field that names a project file (`what` labels the
/// errors: "ops_file", "reference.image").
pub(crate) fn resolve_as(
    project: Option<&Path>,
    file: &str,
    what: &str,
) -> Result<PathBuf, String> {
    let Some(project) = project else {
        return Err(format!(
            "{what} `{file}`: this call has no project to read it from ({what} reads a file inside the caller's game project)"
        ));
    };
    let root = project
        .canonicalize()
        .map_err(|e| format!("{what} `{file}`: project {}: {e}", project.display()))?;
    let path = root.join(file);
    let path = path
        .canonicalize()
        .map_err(|e| format!("{what} `{file}`: {e} (looked for {})", path.display()))?;
    if !path.starts_with(&root) {
        return Err(format!(
            "{what} `{file}` is outside the project {} (it must be a file inside it)",
            root.display()
        ));
    }
    Ok(path)
}

/// The items of an ops file: the same JSON array `ops` takes.
pub(crate) fn read(project: Option<&Path>, file: &str) -> Result<Vec<Value>, String> {
    let path = resolve(project, file)?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("ops_file `{file}`: {e}"))?;
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Array(items)) => Ok(items),
        Ok(_) => Err(format!(
            "ops_file `{file}` must hold a JSON array of items, like `ops`"
        )),
        Err(e) => Err(format!("ops_file `{file}` is not valid JSON: {e}")),
    }
}

/// Says which items came from the file in a batch error: "batch item #N"
/// counts inline items first, then the file's.
pub(crate) fn locate_error(msg: &str, file: &str, from_file: usize, inline: usize) -> String {
    if from_file == 0 {
        return msg.to_string();
    }
    let note = if inline == 0 {
        format!("every item is from ops_file `{file}` (batch item #N = its item #N)")
    } else {
        format!(
            "items #0..#{} are from ops_file `{file}`; later ones are inline (batch item #N = inline item #(N - {from_file}))",
            from_file - 1
        )
    };
    format!("{msg}\n({note})")
}

/// Warnings for ops one call upserts more than once (same `type` + `name`):
/// only the last one survives, which silently undid inline fixes when the
/// file came last. The batch is file items first, then inline, so the
/// inline tweak wins; each line names the op and the source that won.
/// `material` items are left out: in-place materials stack by design.
pub(crate) fn duplicate_warnings(batch: &[Value], from_file: usize) -> Vec<String> {
    let source = |i: usize| if i < from_file { "ops_file" } else { "inline" };
    let mut seen: std::collections::BTreeMap<(String, String), Vec<usize>> = Default::default();
    let mut order = Vec::new();
    for (i, v) in batch.iter().enumerate() {
        let (Some(t), Some(n)) = (
            v.get("type").and_then(Value::as_str),
            v.get("name").and_then(Value::as_str),
        ) else {
            continue;
        };
        if t == "material" {
            continue;
        }
        let key = (t.to_string(), n.to_string());
        let e = seen.entry(key.clone()).or_default();
        if e.is_empty() {
            order.push(key);
        }
        e.push(i);
    }
    order
        .into_iter()
        .filter_map(|key| {
            let at = &seen[&key];
            let (&last, rest) = at.split_last()?;
            if rest.is_empty() {
                return None;
            }
            let others: Vec<String> = rest
                .iter()
                .map(|&i| format!("#{i} ({})", source(i)))
                .collect();
            Some(format!(
                "warning: {} `{}` is given {} times in this call; item #{last} ({}) won, replacing {}",
                key.0,
                key.1,
                at.len(),
                source(last),
                others.join(", ")
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("modeler-ops-file-{}", std::process::id()));
        let _ = std::fs::create_dir_all(dir.join("gen"));
        dir
    }

    #[test]
    fn reads_an_array_inside_the_project() {
        let p = project();
        let _ = std::fs::write(p.join("gen/a.json"), r#"[{"type": "primitive"}]"#);
        let Ok(items) = read(Some(&p), "gen/a.json") else {
            panic!("should read");
        };
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn refuses_outside_the_project_and_no_project() {
        let p = project();
        let outside = p.parent().map(|d| d.join("modeler-ops-outside.json"));
        if let Some(o) = &outside {
            let _ = std::fs::write(o, "[]");
        }
        let err = read(Some(&p), "../modeler-ops-outside.json").err();
        assert!(err.is_some_and(|e| e.contains("outside the project")));
        if let Some(o) = &outside {
            let abs = o.to_string_lossy().to_string();
            assert!(read(Some(&p), &abs).is_err());
        }
        assert!(read(None, "gen/a.json").is_err_and(|e| e.contains("no project")));
        // With no project, an absolute path is read as given.
        let abs = p.join("gen/a.json").to_string_lossy().to_string();
        assert!(read(None, &abs).is_ok());
    }

    #[test]
    fn refuses_non_arrays_and_bad_json() {
        let p = project();
        let _ = std::fs::write(p.join("gen/obj.json"), r#"{"ops": []}"#);
        let _ = std::fs::write(p.join("gen/bad.json"), "[{");
        assert!(read(Some(&p), "gen/obj.json").is_err_and(|e| e.contains("JSON array")));
        assert!(read(Some(&p), "gen/bad.json").is_err_and(|e| e.contains("not valid JSON")));
    }

    #[test]
    fn errors_say_which_items_came_from_the_file() {
        let m = locate_error("batch item #5 failed: no part `x`", "gen/a.json", 10, 2);
        assert!(m.contains("items #0..#9 are from ops_file `gen/a.json`"));
        assert!(m.contains("#(N - 10)"));
        let m = locate_error("batch item #0 failed", "gen/a.json", 3, 0);
        assert!(m.contains("every item is from ops_file"));
    }

    #[test]
    fn duplicate_upserts_warn_and_name_the_winner() {
        let batch: Vec<Value> = serde_json::from_str(
            r#"[{"type": "primitive", "name": "a"}, {"type": "primitive", "name": "b"},
                {"type": "material", "name": "a"}, {"type": "material", "name": "a"},
                {"type": "primitive", "name": "a"}, {"type": "extrude", "name": "b"}]"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let w = duplicate_warnings(&batch, 2);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("primitive `a`"), "{}", w[0]);
        assert!(w[0].contains("item #4 (inline) won"), "{}", w[0]);
        assert!(w[0].contains("#0 (ops_file)"), "{}", w[0]);
    }
}
