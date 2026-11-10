//! The prefab catalog: saved models live as JSON files in an app-data directory with a
//! `catalog.json` index describing each one for the AI (description, tags, prompt).
//! All IO is behind the public functions so tests can point `FRIENDSLOP_DATA_DIR` at a tempdir.

use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::{Model, ModelerError};

/// Where prefabs live: `<data>/friendslop/modeler/prefabs (kept from the friendslop days so existing catalogs carry over)`.
pub fn catalog_dir() -> Result<PathBuf, ModelerError> {
    let base = if let Ok(dir) = std::env::var("FRIENDSLOP_DATA_DIR") {
        if !dir.is_empty() {
            PathBuf::from(dir)
        } else {
            native_data_dir()?
        }
    } else {
        native_data_dir()?
    };
    Ok(base.join("friendslop").join("modeler").join("prefabs"))
}

fn native_data_dir() -> Result<PathBuf, ModelerError> {
    if cfg!(target_os = "macos") {
        let home = std::env::var("HOME").map_err(|_| ModelerError::Io("HOME is not set".into()))?;
        return Ok(PathBuf::from(home)
            .join("Library")
            .join("Application Support"));
    }
    if let Ok(dir) = std::env::var("XDG_DATA_HOME")
        && !dir.is_empty()
    {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var("HOME").map_err(|_| ModelerError::Io("HOME is not set".into()))?;
    Ok(PathBuf::from(home).join(".local").join("share"))
}

fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let trimmed = cleaned.trim_matches('-').to_lowercase();
    if trimmed.is_empty() {
        "model".to_string()
    } else {
        trimmed
    }
}

fn iso_now() -> String {
    // ISO-ish local timestamp, second precision, UTC-offset free. Native-only crate, so
    // SystemTime is fine here.
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    let rem = secs % 86_400;
    // Civil-from-days algorithm (Howard Hinnant) for a readable YYYY-MM-DD.
    let z = (days + 719_468) as i64;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// One entry of the catalog index.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    /// The prompt that produced this prefab.
    pub prompt: String,
    /// Relative `.json` prefab filename inside the catalog dir.
    pub file: String,
    pub created_at: String,
}

/// The `catalog.json` index.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Catalog {
    pub entries: Vec<CatalogEntry>,
}

fn catalog_path() -> Result<PathBuf, ModelerError> {
    Ok(catalog_dir()?.join("catalog.json"))
}

fn load_catalog() -> Result<Catalog, ModelerError> {
    let path = catalog_path()?;
    let Ok(text) = fs::read_to_string(&path) else {
        return Ok(Catalog::default());
    };
    serde_json::from_str(&text)
        .map_err(|e| ModelerError::Parse(format!("parsing {}: {e}", path.display())))
}

fn save_catalog(catalog: &Catalog) -> Result<(), ModelerError> {
    let path = catalog_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| ModelerError::Io(format!("creating {}: {e}", parent.display())))?;
    }
    let text =
        serde_json::to_string_pretty(catalog).map_err(|e| ModelerError::Parse(e.to_string()))?;
    atomic_write(&path, text.as_bytes())
}

/// Writes `bytes` to a sibling `.tmp` file and renames it over `path`, so a
/// crash mid-write never leaves a torn file behind.
fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> Result<(), ModelerError> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, bytes)
        .map_err(|e| ModelerError::Io(format!("writing {}: {e}", tmp.display())))?;
    fs::rename(&tmp, path).map_err(|e| ModelerError::Io(format!("renaming {}: {e}", tmp.display())))
}

/// Where the session autosave lives: `session.json` next to the prefabs.
fn session_state_path() -> Result<PathBuf, ModelerError> {
    Ok(catalog_dir()?.join("session.json"))
}

/// Autosaves the whole session model — parts, ops, history tree, annotations,
/// everything — so an agent's unnamed work-in-progress survives a server
/// restart without an explicit `save_prefab`. Atomic like prefab saves.
/// Failures are reported but never fatal: losing an autosave must not break
/// the tool call that triggered it.
pub fn save_session_state(model: &Model) -> Result<(), ModelerError> {
    let path = session_state_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| ModelerError::Io(format!("creating {}: {e}", parent.display())))?;
    }
    // Compact: the autosave carries every history snapshot, so pretty
    // printing would multiply its size for nobody's benefit.
    let text = serde_json::to_string(model).map_err(|e| ModelerError::Parse(e.to_string()))?;
    atomic_write(&path, text.as_bytes())
}

/// The last autosaved session model, if one exists and parses. Missing or
/// corrupt state just means a fresh session.
pub fn load_session_state() -> Option<Model> {
    let path = session_state_path().ok()?;
    let text = fs::read_to_string(path).ok()?;
    let model: Model = serde_json::from_str(&text).ok()?;
    if let Err(e) = model.check_format() {
        eprintln!("ignoring session autosave: {e}");
        return None;
    }
    Some(model)
}

/// Serialize the model into the catalog dir and upsert its index entry (matched by name).
/// Returns the sanitized model name.
/// Saves a model non-destructively: procedural parts are not written (they are
/// the cached evaluation of the ops and re-derive on load), the history tree
/// rides along inside the model, and the file is replaced atomically so a
/// crash mid-save can't corrupt the prefab.
/// Saves a model non-destructively. Returns the sanitized entry name and
/// whether an existing entry was overwritten.
pub fn save_prefab(
    model: &Model,
    description: &str,
    tags: &[&str],
    prompt: &str,
) -> Result<(String, bool), ModelerError> {
    let name = sanitize(&model.name);
    let mut catalog = load_catalog()?;
    let overwrote = catalog.entries.iter().any(|e| e.name == name);
    let mut saved = model.clone();
    saved.name = name.clone();
    if saved.parts_derive_from_ops() {
        saved.parts.clear();
    }
    let dir = catalog_dir()?;
    fs::create_dir_all(&dir)
        .map_err(|e| ModelerError::Io(format!("creating {}: {e}", dir.display())))?;
    let file = format!("{name}.json");
    let text =
        serde_json::to_string_pretty(&saved).map_err(|e| ModelerError::Parse(e.to_string()))?;
    let path = dir.join(&file);
    atomic_write(&path, text.as_bytes())?;

    let entry = CatalogEntry {
        name: name.clone(),
        description: description.to_string(),
        tags: tags.iter().map(|t| t.to_string()).collect(),
        prompt: prompt.to_string(),
        file,
        created_at: iso_now(),
    };
    match catalog.entries.iter_mut().find(|e| e.name == name) {
        Some(existing) => *existing = entry,
        None => catalog.entries.push(entry),
    }
    save_catalog(&catalog)?;
    Ok((name, overwrote))
}

/// Load a saved model by name. Procedural prefabs store no parts; they are
/// re-derived from the ops here, so a graph prefab is small on disk and still
/// opens fully evaluated.
pub fn load_prefab(name: &str) -> Result<Model, ModelerError> {
    let path = catalog_dir()?.join(format!("{}.json", sanitize(name)));
    let text = fs::read_to_string(&path)
        .map_err(|e| ModelerError::Io(format!("reading {}: {e}", path.display())))?;
    let mut model: Model = serde_json::from_str(&text)
        .map_err(|e| ModelerError::Parse(format!("parsing {}: {e}", path.display())))?;
    model.check_format()?;
    // Only re-derive what was saved without parts; models that kept their
    // parts (baked edits on top of ops) must not lose those edits.
    model.restore_parts()?;
    // Saved snapshots store no nested history, but their ops are intact, so
    // bring each one's parts back too.
    for node in &mut model.history.nodes {
        node.model.restore_parts()?;
    }
    Ok(model)
}

/// List all catalog entries, most recent last.
pub fn list() -> Result<Vec<CatalogEntry>, ModelerError> {
    let mut entries = load_catalog()?.entries;
    entries.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    Ok(entries)
}

/// A compact human/LLM-readable description of one entry.
pub fn describe(name: &str) -> Result<String, ModelerError> {
    let catalog = load_catalog()?;
    let wanted = sanitize(name);
    let entry = catalog
        .entries
        .iter()
        .find(|e| e.name == wanted)
        .ok_or_else(|| format!("no prefab named '{wanted}' in the catalog"))?;
    Ok(format!(
        "name: {}\ndescription: {}\ntags: {}\nprompt: {}\nfile: {}\ncreated: {}",
        entry.name,
        entry.description,
        entry.tags.join(", "),
        entry.prompt,
        entry.file,
        entry.created_at,
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// FRIENDSLOP_DATA_DIR is process-global, so tests that set it must not
    /// run in parallel with each other.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "friendslop-modeler-catalog-{tag}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn session_state_roundtrip_and_missing_file() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: test-only, single-threaded; edition 2024 makes set_var unsafe.
        unsafe { std::env::set_var("FRIENDSLOP_DATA_DIR", temp_root("state")) };
        assert!(
            load_session_state().is_none(),
            "no autosave yet -> fresh session"
        );
        let mut model = Model {
            name: "wip".to_string(),
            ..Default::default()
        };
        model.ops.push(crate::Op::Primitive {
            name: "leg".into(),
            kind: crate::PrimitiveKind::Box,
            size: [1.0, 1.0, 1.0],
            translation: [0.0, 0.5, 0.0],
            rotation: None,
        });
        model.refresh().unwrap();
        save_session_state(&model).unwrap();

        let restored = load_session_state().unwrap();
        assert_eq!(restored.name, "wip");
        assert_eq!(restored.ops.len(), 1);
        assert_eq!(restored.parts.len(), 1, "parts are kept, not re-derived");
        // Overwrite works (atomic tmp+rename).
        model.name = "wip2".into();
        save_session_state(&model).unwrap();
        assert_eq!(load_session_state().unwrap().name, "wip2");
    }

    #[test]
    fn save_list_load_describe_roundtrip() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: test-only, single-threaded; edition 2024 makes set_var unsafe.
        unsafe { std::env::set_var("FRIENDSLOP_DATA_DIR", temp_root("roundtrip")) };
        let mut model = Model {
            name: "Red Box".to_string(),
            ..Default::default()
        };
        let mut part = crate::Part::new("box");
        part.verts = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        part.indices = vec![0, 1, 2];
        model.parts.push(part.into());

        let (saved, overwrote) =
            save_prefab(&model, "A red box", &["box", "red"], "make a red box").unwrap();
        assert_eq!(saved, "red-box");
        assert!(!overwrote, "first save is not an overwrite");

        let entries = list().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].description, "A red box");

        let loaded = load_prefab("Red Box").unwrap();
        assert_eq!(loaded.name, "red-box");
        assert_eq!(loaded.parts.len(), 1);
        assert_eq!(loaded.parts[0].name, "box");

        let text = describe("red box").unwrap();
        assert!(text.contains("description: A red box"));
        assert!(text.contains("tags: box, red"));

        // Upsert: saving again doesn't grow the catalog.
        let (_, overwrote) = save_prefab(&model, "Still a red box", &["box"], "again").unwrap();
        assert!(overwrote, "second save overwrites");
        assert_eq!(list().unwrap().len(), 1);
        assert_eq!(list().unwrap()[0].description, "Still a red box");
    }
}
