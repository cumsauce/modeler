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
pub(crate) fn resolve(project: Option<&Path>, file: &str) -> Result<PathBuf, String> {
    let Some(project) = project else {
        return Err(format!(
            "ops_file `{file}`: this call has no project to read it from (ops_file reads a file inside the caller's game project)"
        ));
    };
    let root = project
        .canonicalize()
        .map_err(|e| format!("ops_file `{file}`: project {}: {e}", project.display()))?;
    let path = root.join(file);
    let path = path
        .canonicalize()
        .map_err(|e| format!("ops_file `{file}`: {e} (looked for {})", path.display()))?;
    if !path.starts_with(&root) {
        return Err(format!(
            "ops_file `{file}` is outside the project {} (it must be a file inside it)",
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
pub(crate) fn locate_error(msg: &str, file: &str, inline: usize, from_file: usize) -> String {
    if from_file == 0 {
        return msg.to_string();
    }
    let note = if inline == 0 {
        format!("every item is from ops_file `{file}` (batch item #N = its item #N)")
    } else {
        format!(
            "items #{inline}..#{} are from ops_file `{file}` (batch item #N = its item #(N - {inline})); earlier ones are inline",
            inline + from_file - 1
        )
    };
    format!("{msg}\n({note})")
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
        let m = locate_error("batch item #5 failed: no part `x`", "gen/a.json", 2, 10);
        assert!(m.contains("items #2..#11 are from ops_file `gen/a.json`"));
        assert!(m.contains("#(N - 2)"));
        let m = locate_error("batch item #0 failed", "gen/a.json", 0, 3);
        assert!(m.contains("every item is from ops_file"));
    }
}
