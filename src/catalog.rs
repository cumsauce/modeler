//! The prefab catalog: saved models live as JSON files in an app-data directory with a
//! `catalog.json` index describing each one for the AI (description, tags, prompt).
//! All IO is behind the public functions so tests can point `FRIENDSLOP_DATA_DIR` at a tempdir.

use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::{Model, ModelerError};

/// A library kind: each lives in its own folder with its own `catalog.json`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Prefabs,
    Materials,
    Styles,
}

impl Kind {
    pub fn dir_name(self) -> &'static str {
        match self {
            Kind::Prefabs => "prefabs",
            Kind::Materials => "materials",
            Kind::Styles => "styles",
        }
    }
    fn noun(self) -> &'static str {
        match self {
            Kind::Prefabs => "prefab",
            Kind::Materials => "material",
            Kind::Styles => "style",
        }
    }
}

/// Where prefabs live: `<data>/friendslop/modeler/prefabs (kept from the friendslop days so existing catalogs carry over)`.
pub fn catalog_dir() -> Result<PathBuf, ModelerError> {
    kind_dir(Kind::Prefabs)
}

/// The folder of one library kind: `<data>/friendslop/modeler/<kind>`.
pub fn kind_dir(kind: Kind) -> Result<PathBuf, ModelerError> {
    let base = if let Ok(dir) = std::env::var("FRIENDSLOP_DATA_DIR") {
        if !dir.is_empty() {
            PathBuf::from(dir)
        } else {
            native_data_dir()?
        }
    } else {
        native_data_dir()?
    };
    Ok(base
        .join("friendslop")
        .join("modeler")
        .join(kind.dir_name()))
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

/// The knobs a saved model exposes, read from its file without evaluating
/// it: named params (with values) and named moves/places, so other agents
/// can tune a prefab instead of rebuilding it.
pub fn knobs(entry: &CatalogEntry) -> String {
    let Ok(dir) = catalog_dir() else {
        return String::new();
    };
    let Ok(text) = fs::read_to_string(dir.join(&entry.file)) else {
        return String::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return String::new();
    };
    let mut out = Vec::new();
    if let Some(p) = v["params"].as_object().filter(|p| !p.is_empty()) {
        let list: Vec<String> = p.iter().map(|(k, v)| format!("{k}={v}")).collect();
        out.push(format!("params {}", list.join(", ")));
    }
    let moves: Vec<String> = v["ops"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| o.get("move"))
        .filter_map(|m| {
            let n = m["name"].as_str()?;
            Some(match m["on"].as_str() {
                Some(on) => format!("{n} (place on {on})"),
                None => n.to_string(),
            })
        })
        .collect();
    if !moves.is_empty() {
        out.push(format!("moves {}", moves.join(", ")));
    }
    out.join("; ")
}

/// The `catalog.json` index.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Catalog {
    pub entries: Vec<CatalogEntry>,
}

fn catalog_path(kind: Kind) -> Result<PathBuf, ModelerError> {
    Ok(kind_dir(kind)?.join("catalog.json"))
}

fn load_catalog() -> Result<Catalog, ModelerError> {
    load_index(Kind::Prefabs)
}

fn load_index(kind: Kind) -> Result<Catalog, ModelerError> {
    let path = catalog_path(kind)?;
    let Ok(text) = fs::read_to_string(&path) else {
        return Ok(Catalog::default());
    };
    serde_json::from_str(&text)
        .map_err(|e| ModelerError::Parse(format!("parsing {}: {e}", path.display())))
}

fn save_catalog(catalog: &Catalog) -> Result<(), ModelerError> {
    save_index(Kind::Prefabs, catalog)
}

fn save_index(kind: Kind, catalog: &Catalog) -> Result<(), ModelerError> {
    let path = catalog_path(kind)?;
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
pub(crate) fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> Result<(), ModelerError> {
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

/// Autosaves any per-kind session state (e.g. the working materials) as
/// `session.json` in that kind's folder.
pub fn save_kind_state<T: Serialize>(kind: Kind, state: &T) -> Result<(), ModelerError> {
    let dir = kind_dir(kind)?;
    fs::create_dir_all(&dir)
        .map_err(|e| ModelerError::Io(format!("creating {}: {e}", dir.display())))?;
    let text = serde_json::to_string(state).map_err(|e| ModelerError::Parse(e.to_string()))?;
    atomic_write(&dir.join("session.json"), text.as_bytes())
}

/// The autosaved per-kind session state, if present and parseable.
pub fn load_kind_state<T: serde::de::DeserializeOwned>(kind: Kind) -> Option<T> {
    let text = fs::read_to_string(kind_dir(kind).ok()?.join("session.json")).ok()?;
    serde_json::from_str(&text).ok()
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

/// Saves any library document (a material graph, ...) under `name` in
/// `kind`'s folder and upserts its index entry. The document is written as
/// [`crate::Doc::for_file`] (trimmed, its history kept inside it).
/// Returns the sanitized name and whether an entry was overwritten.
pub fn save_doc<T: crate::Doc>(
    kind: Kind,
    name: &str,
    doc: &T,
    description: &str,
    tags: &[&str],
    prompt: &str,
) -> Result<(String, bool), ModelerError> {
    let name = sanitize(name);
    let mut catalog = load_index(kind)?;
    let overwrote = catalog.entries.iter().any(|e| e.name == name);
    let dir = kind_dir(kind)?;
    fs::create_dir_all(&dir)
        .map_err(|e| ModelerError::Io(format!("creating {}: {e}", dir.display())))?;
    let file = format!("{name}.json");
    let text = serde_json::to_string_pretty(doc).map_err(|e| ModelerError::Parse(e.to_string()))?;
    atomic_write(&dir.join(&file), text.as_bytes())?;
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
    save_index(kind, &catalog)?;
    Ok((name, overwrote))
}

/// Loads a library document saved with [`save_doc`].
pub fn load_doc<T: crate::Doc>(kind: Kind, name: &str) -> Result<T, ModelerError> {
    let path = kind_dir(kind)?.join(format!("{}.json", sanitize(name)));
    let text = fs::read_to_string(&path).map_err(|_| {
        ModelerError::Io(format!(
            "no {} named '{}' in the library",
            kind.noun(),
            sanitize(name)
        ))
    })?;
    serde_json::from_str(&text)
        .map_err(|e| ModelerError::Parse(format!("parsing {}: {e}", path.display())))
}

/// Entries of one library kind, most recent last.
pub fn list_kind(kind: Kind) -> Result<Vec<CatalogEntry>, ModelerError> {
    let mut entries = load_index(kind)?.entries;
    entries.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    Ok(entries)
}

/// One entry of a library kind.
pub fn entry(kind: Kind, name: &str) -> Result<CatalogEntry, ModelerError> {
    let wanted = sanitize(name);
    load_index(kind)?
        .entries
        .into_iter()
        .find(|e| e.name == wanted)
        .ok_or_else(|| format!("no {} named '{wanted}' in the library", kind.noun()).into())
}

/// Removes a document and its entry (the file goes to `<kind>/removed/`,
/// so a delete can be undone by hand).
pub fn delete_doc(kind: Kind, name: &str) -> Result<(), ModelerError> {
    let wanted = sanitize(name);
    let mut catalog = load_index(kind)?;
    let Some(i) = catalog.entries.iter().position(|e| e.name == wanted) else {
        return Err(format!("no {} named '{wanted}' in the library", kind.noun()).into());
    };
    let e = catalog.entries.remove(i);
    let dir = kind_dir(kind)?;
    let bin = dir.join("removed");
    fs::create_dir_all(&bin)
        .map_err(|e| ModelerError::Io(format!("creating {}: {e}", bin.display())))?;
    let _ = fs::rename(dir.join(&e.file), bin.join(&e.file));
    save_index(kind, &catalog)
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
        "name: {}\ndescription: {}\ntags: {}\nknobs: {}\nprompt: {}\nfile: {}\ncreated: {}",
        entry.name,
        entry.description,
        entry.tags.join(", "),
        {
            let k = knobs(entry);
            if k.is_empty() { "none".to_string() } else { k }
        },
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

    #[test]
    fn prefab_op_instances_a_saved_model() {
        use serde_json::json;
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: test-only, serialized by ENV_LOCK.
        unsafe { std::env::set_var("FRIENDSLOP_DATA_DIR", temp_root("prefab-op")) };
        let mut s = crate::commands::Session::default();
        s.model.name = "lamp".into();
        s.apply_values(
            None,
            vec![
                json!({"type": "primitive", "name": "pole", "kind": "cylinder", "size": [0.05, 1], "translation": [0, 0.5, 0]}),
                json!({"type": "primitive", "name": "shade", "kind": "cone", "size": [0.3, 0.3], "translation": [0, 1.1, 0]}),
                json!({"type": "material", "name": "shade", "color": [1, 0.9, 0.5]}),
            ],
        )
        .unwrap();
        save_prefab(&s.model, "a lamp", &["test"], "lamp").unwrap();
        let mut scene = crate::commands::Session::default();
        scene
            .apply_values(
                None,
                vec![
                    json!({"type": "prefab", "name": "l1", "prefab": "lamp", "translation": [2, 0, 0]}),
                    json!({"type": "prefab", "name": "l2", "prefab": "lamp", "translation": [-2, 0, 0], "scale": [2, 2, 2]}),
                ],
            )
            .unwrap();
        let shade = scene.model.part("l2.shade").unwrap();
        assert_eq!(shade.translation, [-2.0, 2.2, 0.0]);
        assert_eq!(shade.material.base_color, [1.0, 0.9, 0.5, 1.0]);
        assert!(scene.model.part("l1.pole").is_some());
        // Instanced parts feed later ops like any other part.
        scene
            .apply_values(
                None,
                vec![json!({"type": "linear_array", "name": "row", "input": "l1.pole", "count": 3, "offset": [0, 0, 1]})],
            )
            .unwrap();
        assert!(scene.model.part("row_2").is_some());
    }
}
