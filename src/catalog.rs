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

/// The folder of one library kind. In a game project (see [`project_root`])
/// that is `<project>/.modeler/<kind>`, outside `assets/` so the editable
/// sources ship with the repo but not with the game; otherwise the shared
/// `<data>/friendslop/modeler/<kind>`. `FRIENDSLOP_DATA_DIR` overrides both
/// (tests).
pub fn kind_dir(kind: Kind) -> Result<PathBuf, ModelerError> {
    if let Ok(dir) = std::env::var("FRIENDSLOP_DATA_DIR")
        && !dir.is_empty()
    {
        return Ok(PathBuf::from(dir)
            .join("friendslop")
            .join("modeler")
            .join(kind.dir_name()));
    }
    if let Some(root) = project_root() {
        return Ok(root.join(PROJECT_DIR).join(kind.dir_name()));
    }
    Ok(native_data_dir()?
        .join("friendslop")
        .join("modeler")
        .join(kind.dir_name()))
}

/// The per-project folder holding prefabs, styles, materials and the session.
pub const PROJECT_DIR: &str = ".modeler";

/// The game project this server works in, if any: `MODELER_PROJECT` (set by
/// `--project <dir>`), else the nearest ancestor of the working directory
/// that has a `.modeler/` folder, or a `Cargo.toml` next to an `assets/`
/// folder (a Bevy game). None = the shared library.
pub fn project_root() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("MODELER_PROJECT")
        && !dir.is_empty()
    {
        return Some(PathBuf::from(dir));
    }
    let cwd = std::env::current_dir().ok()?;
    find_project(&cwd)
}

/// [`project_root`]'s search from `start` upward.
pub fn find_project(start: &std::path::Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|d| {
            d.join(PROJECT_DIR).is_dir()
                || (d.join("Cargo.toml").is_file() && d.join("assets").is_dir())
        })
        .map(PathBuf::from)
}

/// Where relative export paths land: the project's `assets/` (created on
/// demand), or the working directory outside a project.
pub fn resolve_export_path(path: &std::path::Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    match project_root() {
        Some(root) => root.join("assets").join(path),
        None => path.to_path_buf(),
    }
}

/// Keeps per-machine autosaves out of the project's git history (written
/// once, when `.modeler/` is first created; prefabs, styles and materials
/// stay tracked).
fn ensure_project_gitignore() {
    let Some(root) = project_root() else {
        return;
    };
    let ign = root.join(PROJECT_DIR).join(".gitignore");
    if ign.exists() || std::env::var("FRIENDSLOP_DATA_DIR").is_ok_and(|d| !d.is_empty()) {
        return;
    }
    let _ = fs::create_dir_all(root.join(PROJECT_DIR));
    let _ = fs::write(
        ign,
        "# modeler autosaves (per machine); prefabs/styles/materials are the sources\n\
         */session.json\n*/open_docs.json\n*/session.*.json\n*/open_docs.*.json\n*/session.lock\n*/daemon.log\n*/daemon.log.1\n\
         *.tmp\n*/*.tmp\n",
    );
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
    ensure_project_gitignore();
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

/// Where the session autosave lives: `session.json` next to the prefabs, or
/// `session.<slot>.json` while another server owns the main autosave.
fn session_state_path() -> Result<PathBuf, ModelerError> {
    Ok(catalog_dir()?.join(autosave_file("session")))
}

/// This server's own autosave name while another server holds the session
/// lock: `MODELER_SLOT` (the driver passes one per daemon socket, so a
/// restarted daemon finds its work again), else `pid<pid>`.
pub fn autosave_slot() -> String {
    std::env::var("MODELER_SLOT")
        .ok()
        .map(|s| {
            s.chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
                .collect::<String>()
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("pid{}", std::process::id()))
}

/// `<stem>.json` for the autosave owner, `<stem>.<slot>.json` otherwise.
fn autosave_file(stem: &str) -> String {
    match autosave_owner() {
        None => format!("{stem}.json"),
        Some(_) => format!("{stem}.{}.json", autosave_slot()),
    }
}

/// Reads an autosave: this server's slot file when it does not own the main
/// autosave and the slot file exists (a restarted secondary daemon), else
/// the main file.
fn read_autosave(dir: &std::path::Path, stem: &str) -> Option<String> {
    if autosave_owner().is_some()
        && let Ok(t) = fs::read_to_string(dir.join(format!("{stem}.{}.json", autosave_slot())))
    {
        return Some(t);
    }
    fs::read_to_string(dir.join(format!("{stem}.json"))).ok()
}

/// Where this server autosaves, for notes: `None` = the main `session.json`.
pub fn autosave_note() -> Option<String> {
    let owner = autosave_owner()?;
    Some(format!(
        "server pid {owner} owns the main autosave, so this server autosaves to its own session.{}.json (restored when this server restarts with the same slot); save_prefab to share work",
        autosave_slot()
    ))
}

/// Autosaves the whole session model — parts, ops, history tree, annotations,
/// everything — so an agent's unnamed work-in-progress survives a server
/// restart without an explicit `save_prefab`. Atomic like prefab saves.
/// Failures are reported but never fatal: losing an autosave must not break
/// the tool call that triggered it.
pub fn save_session_state(model: &Model) -> Result<(), ModelerError> {
    ensure_project_gitignore();
    let path = session_state_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| ModelerError::Io(format!("creating {}: {e}", parent.display())))?;
    }
    // Compact: the autosave carries every history snapshot, so pretty
    // printing would multiply its size for nobody's benefit.
    let text = serde_json::to_string(&slim(model).as_ref())
        .map_err(|e| ModelerError::Parse(e.to_string()))?;
    atomic_write(&path, text.as_bytes())
}

/// The model as autosaved: without its cached parts when they all derive
/// from the ops (re-derived on restore, like prefabs). The parts were most
/// of the bytes (3.9 of 4 MB for the duck), rewritten on every edit.
fn slim(model: &Model) -> std::borrow::Cow<'_, Model> {
    if model.parts_derive_from_ops() && !model.parts.is_empty() {
        let mut m = model.clone();
        m.parts.clear();
        std::borrow::Cow::Owned(m)
    } else {
        std::borrow::Cow::Borrowed(model)
    }
}

/// Autosaves any per-kind session state (e.g. the working materials) as
/// `session.json` in that kind's folder.
pub fn save_kind_state<T: Serialize>(kind: Kind, state: &T) -> Result<(), ModelerError> {
    let dir = kind_dir(kind)?;
    fs::create_dir_all(&dir)
        .map_err(|e| ModelerError::Io(format!("creating {}: {e}", dir.display())))?;
    let text = serde_json::to_string(state).map_err(|e| ModelerError::Parse(e.to_string()))?;
    atomic_write(&dir.join(autosave_file("session")), text.as_bytes())
}

/// The autosaved per-kind session state, if present and parseable.
pub fn load_kind_state<T: serde::de::DeserializeOwned>(kind: Kind) -> Option<T> {
    let text = read_autosave(&kind_dir(kind).ok()?, "session")?;
    serde_json::from_str(&text).ok()
}

/// Autosaves the inactive open documents (`open_docs.json`), so a restart
/// restores every open scene, not just the active one.
pub fn save_open_docs(docs: &[Model]) -> Result<(), ModelerError> {
    let dir = catalog_dir()?;
    fs::create_dir_all(&dir)
        .map_err(|e| ModelerError::Io(format!("creating {}: {e}", dir.display())))?;
    let slim: Vec<std::borrow::Cow<'_, Model>> = docs.iter().map(slim).collect();
    let text = serde_json::to_string(&slim).map_err(|e| ModelerError::Parse(e.to_string()))?;
    atomic_write(&dir.join(autosave_file("open_docs")), text.as_bytes())
}

/// The autosaved inactive documents (empty when missing or unreadable).
pub fn load_open_docs() -> Vec<Model> {
    let Ok(dir) = catalog_dir() else {
        return Vec::new();
    };
    let Some(text) = read_autosave(&dir, "open_docs") else {
        return Vec::new();
    };
    let docs: Vec<Model> = serde_json::from_str(&text).unwrap_or_default();
    docs.into_iter()
        .filter(|m| m.check_format().is_ok())
        .map(|mut m| {
            let _ = m.restore_parts();
            m
        })
        .collect()
}

/// Who may autosave: the outcome of taking `session.lock`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockOutcome {
    /// This process holds the lock and autosaves.
    Owner,
    /// Another live server (this pid) holds it; this one does not autosave.
    HeldBy(u32),
}

/// The lock this process took at startup ([`acquire_session_lock`]). `None`
/// (tests, library use) means autosave as usual.
static SESSION_LOCK: std::sync::Mutex<Option<(PathBuf, LockOutcome)>> = std::sync::Mutex::new(None);

/// `session.lock` next to the autosave: the pid of the server that owns it.
pub fn session_lock_path() -> Result<PathBuf, ModelerError> {
    Ok(catalog_dir()?.join("session.lock"))
}

/// Whether a process with this pid exists (a pid we may not signal still
/// counts as alive).
pub fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        if pid <= 0 {
            return false;
        }
        // SAFETY: signal 0 sends nothing; it only checks that the pid exists.
        let r = unsafe { libc::kill(pid, 0) };
        r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

fn lock_holder(path: &std::path::Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

/// Takes the lock file at `path` for `pid` unless another live pid holds it.
/// A lock left by a dead process is taken over. The file appears with its
/// content in one step (hard link of a written temp file), so a reader never
/// sees it empty.
pub fn acquire_lock_at(path: &std::path::Path, pid: u32) -> Result<LockOutcome, ModelerError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| ModelerError::Io(format!("creating {}: {e}", parent.display())))?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{pid}.tmp"));
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, format!("{pid}\n"))
        .map_err(|e| ModelerError::Io(format!("writing {}: {e}", tmp.display())))?;
    let mut outcome = Err(ModelerError::Io(format!(
        "could not take {} (it keeps changing hands)",
        path.display()
    )));
    for _ in 0..3 {
        match fs::hard_link(&tmp, path) {
            Ok(()) => {
                outcome = Ok(LockOutcome::Owner);
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => match lock_holder(path) {
                Some(h) if h == pid => {
                    outcome = Ok(LockOutcome::Owner);
                    break;
                }
                Some(h) if pid_alive(h) => {
                    outcome = Ok(LockOutcome::HeldBy(h));
                    break;
                }
                // Dead holder or unreadable file: stale, take it over.
                _ => {
                    let _ = fs::remove_file(path);
                }
            },
            Err(e) => {
                outcome = Err(ModelerError::Io(format!("linking {}: {e}", path.display())));
                break;
            }
        }
    }
    let _ = fs::remove_file(&tmp);
    outcome
}

/// Removes the lock at `path` if `pid` holds it (never another server's).
pub fn release_lock_at(path: &std::path::Path, pid: u32) {
    if lock_holder(path) == Some(pid) {
        let _ = fs::remove_file(path);
    }
}

/// Startup: takes `<catalog dir>/session.lock` for this process, so only one
/// live server autosaves `session.json` / `open_docs.json` into a catalog
/// dir. A second server still works but does not autosave.
pub fn acquire_session_lock() -> Result<LockOutcome, ModelerError> {
    let path = session_lock_path()?;
    let outcome = acquire_lock_at(&path, std::process::id())?;
    let mut g = SESSION_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    *g = Some((path, outcome));
    Ok(outcome)
}

/// The other live server's pid when it owns the autosave and this one does
/// not (`None`: this server autosaves). When that server has since exited,
/// this one takes the lock over and autosaves from then on.
pub fn autosave_owner() -> Option<u32> {
    let mut g = SESSION_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (path, outcome) = g.as_mut()?;
    let LockOutcome::HeldBy(other) = *outcome else {
        return None;
    };
    if pid_alive(other) {
        return Some(other);
    }
    match acquire_lock_at(path, std::process::id()) {
        Ok(o) => *outcome = o,
        Err(_) => return Some(other),
    }
    match *outcome {
        LockOutcome::HeldBy(pid) => Some(pid),
        LockOutcome::Owner => None,
    }
}

/// Clean exit: removes `session.lock` if this process holds it.
pub fn release_session_lock() {
    let g = SESSION_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((path, LockOutcome::Owner)) = g.as_ref() {
        release_lock_at(path, std::process::id());
    }
}

/// The last autosaved session model, if one exists and parses. Missing or
/// corrupt state just means a fresh session.
pub fn load_session_state() -> Option<Model> {
    let text = read_autosave(&catalog_dir().ok()?, "session")?;
    let mut model: Model = serde_json::from_str(&text).ok()?;
    if let Err(e) = model.check_format() {
        eprintln!("ignoring session autosave: {e}");
        return None;
    }
    if let Err(e) = model.restore_parts() {
        eprintln!("session autosave: re-deriving parts failed: {e}");
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
    if let Some(m) = open_doc(name) {
        return Ok(m);
    }
    load_prefab_file(name)
}

static OPEN_DOCS: std::sync::Mutex<Option<std::collections::HashMap<String, Model>>> =
    std::sync::Mutex::new(None);

/// Registers (or with `None` removes) a document open in the session, so
/// prefab instances of that name resolve to the live doc before the file.
pub fn set_open_doc(name: &str, model: Option<Model>) {
    if let Ok(mut g) = OPEN_DOCS.lock() {
        let map = g.get_or_insert_with(Default::default);
        match model {
            Some(m) => {
                map.insert(name.to_string(), m);
            }
            None => {
                map.remove(name);
            }
        }
    }
}

/// The open document named `name`, if any.
pub fn open_doc(name: &str) -> Option<Model> {
    OPEN_DOCS
        .lock()
        .ok()
        .and_then(|g| g.as_ref().and_then(|m| m.get(name).cloned()))
}

/// [`load_prefab`] from the catalog file only, ignoring open documents.
pub fn load_prefab_file(name: &str) -> Result<Model, ModelerError> {
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
            frame: None,
        });
        model.refresh().unwrap();
        save_session_state(&model).unwrap();

        let restored = load_session_state().unwrap();
        assert_eq!(restored.name, "wip");
        assert_eq!(restored.ops.len(), 1);
        assert_eq!(restored.parts.len(), 1, "parts re-derive on restore");
        let raw = fs::read_to_string(session_state_path().unwrap()).unwrap();
        assert!(
            !raw.contains("\"verts\""),
            "derivable parts are not written to the autosave"
        );
        // Overwrite works (atomic tmp+rename).
        model.name = "wip2".into();
        save_session_state(&model).unwrap();
        assert_eq!(load_session_state().unwrap().name, "wip2");
    }

    #[test]
    fn session_lock_second_acquirer_sees_the_first_pid() {
        let dir = temp_root("lock");
        let path = dir.join("session.lock");
        let me = std::process::id();
        assert_eq!(acquire_lock_at(&path, me).unwrap(), LockOutcome::Owner);
        // A second server (another pid) finds us alive and does not own it.
        let other = me.wrapping_add(7919);
        assert_eq!(
            acquire_lock_at(&path, other).unwrap(),
            LockOutcome::HeldBy(me)
        );
        // Re-acquiring our own lock is fine; releasing someone else's is not.
        assert_eq!(acquire_lock_at(&path, me).unwrap(), LockOutcome::Owner);
        release_lock_at(&path, other);
        assert!(path.exists(), "only the holder removes the lock");

        // A lock left by a dead process is stale and taken over.
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead = child.id();
        child.wait().unwrap();
        fs::write(&path, format!("{dead}\n")).unwrap();
        assert!(!pid_alive(dead));
        assert_eq!(acquire_lock_at(&path, me).unwrap(), LockOutcome::Owner);
        assert_eq!(lock_holder(&path), Some(me));
        release_lock_at(&path, me);
        assert!(!path.exists());
        let _ = fs::remove_dir_all(&dir);
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

#[cfg(test)]
mod project_tests {
    use super::*;

    #[test]
    fn finds_a_bevy_project_or_a_modeler_folder_upward() {
        let tmp = std::env::temp_dir().join(format!("modeler_proj_{}", std::process::id()));
        let game = tmp.join("game");
        let deep = game.join("src").join("levels");
        fs::create_dir_all(&deep).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(find_project(&deep), None, "no Cargo.toml + assets yet");
        fs::write(game.join("Cargo.toml"), "[package]").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(find_project(&deep), None, "Cargo.toml alone is not a game");
        fs::create_dir_all(game.join("assets")).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(find_project(&deep), Some(game.clone()));
        let other = tmp.join("other");
        fs::create_dir_all(other.join(PROJECT_DIR)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(find_project(&other), Some(other.clone()));
        let _ = fs::remove_dir_all(&tmp);
    }
}
