//! The prefab catalog: saved models live as JSON files in an app-data directory with a
//! `catalog.json` index describing each one for the AI (description, tags, prompt).
//! All IO is behind the public functions so tests can point `FRIENDSLOP_DATA_DIR` at a tempdir.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::{Doc, Model, ModelerError};

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
    kind_dir_for(kind, project_root())
}

/// [`kind_dir`] of the server's own project ([`home_project`]), whatever
/// project the current call works in: autosaves and the session lock are
/// the server's, not a project's.
pub fn home_dir(kind: Kind) -> Result<PathBuf, ModelerError> {
    kind_dir_for(kind, home_project())
}

fn kind_dir_for(kind: Kind, project: Option<PathBuf>) -> Result<PathBuf, ModelerError> {
    if let Ok(dir) = std::env::var("FRIENDSLOP_DATA_DIR")
        && !dir.is_empty()
    {
        return Ok(PathBuf::from(dir)
            .join("friendslop")
            .join("modeler")
            .join(kind.dir_name()));
    }
    if let Some(root) = project {
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
    if let Ok(g) = CALL_PROJECT.read()
        && let Some(p) = g.as_ref()
    {
        return p.clone();
    }
    home_project()
}

/// The project the current call works in, when it says (`Some(None)` = the
/// shared library). One server serves every project: each call names its
/// caller's project and the catalog, exports and prefab lookups follow it.
static CALL_PROJECT: std::sync::RwLock<Option<Option<PathBuf>>> = std::sync::RwLock::new(None);

/// Sets the current call's project (`None`: back to [`home_project`]);
/// returns the previous setting.
pub fn set_call_project(project: Option<Option<PathBuf>>) -> Option<Option<PathBuf>> {
    match CALL_PROJECT.write() {
        Ok(mut g) => std::mem::replace(&mut *g, project),
        Err(_) => None,
    }
}

/// A project's display name: its git repository's folder name, with the
/// path inside the repository when the project is a subfolder
/// (`friendSLOP/crates/game`), else its own folder name.
pub fn project_name(p: &Path) -> String {
    let own = || {
        p.file_name()
            .map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into())
    };
    let Some(repo) = p.ancestors().find(|a| a.join(".git").exists()) else {
        return own();
    };
    let name = repo
        .file_name()
        .map_or_else(own, |n| n.to_string_lossy().into_owned());
    match p.strip_prefix(repo) {
        Ok(rest) if !rest.as_os_str().is_empty() => format!("{name}/{}", rest.display()),
        _ => name,
    }
}

/// The current call's project, when one is set.
pub fn call_project() -> Option<Option<PathBuf>> {
    CALL_PROJECT.read().ok().and_then(|g| g.clone())
}

/// Runs `f` with the catalog in `project` (`None` = the shared library),
/// then puts the previous setting back.
pub fn with_project<R>(project: Option<PathBuf>, f: impl FnOnce() -> R) -> R {
    let old = set_call_project(Some(project));
    let r = f();
    set_call_project(old);
    r
}

/// The server's own project: `MODELER_PROJECT` / `--project`, else the one
/// its working directory is in, else none (the shared library).
pub fn home_project() -> Option<PathBuf> {
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
    if std::env::var("FRIENDSLOP_DATA_DIR").is_ok_and(|d| !d.is_empty()) {
        return;
    }
    // Rules added after the first .gitignore was written: prefab undo
    // histories (sidecars) and per-document session journals, per machine
    // like the autosaves.
    const LATER: [(&str, &str); 2] = [
        ("*/*.history.json", "prefab undo histories (per machine)"),
        (
            "*/session*.jsonl",
            "per-document session journals (per machine)",
        ),
    ];
    if let Ok(text) = fs::read_to_string(&ign) {
        let mut out = text.clone();
        for (rule, why) in LATER {
            if !text.lines().any(|l| l.trim() == rule) {
                if !out.ends_with('\n') {
                    out.push('\n');
                }
                out += &format!("# {why}\n{rule}\n");
            }
        }
        if out != text {
            let _ = fs::write(&ign, out);
        }
        return;
    }
    let _ = fs::create_dir_all(root.join(PROJECT_DIR));
    let _ = fs::write(
        ign,
        "# modeler autosaves (per machine); prefabs/styles/materials are the sources\n\
         */*.history.json\n*/session.json\n*/session*.jsonl\n*/open_docs.json\n*/session.*.json\n*/open_docs.*.json\n*/session.lock\n*/feedback.jsonl\n*/daemon.log\n*/daemon.log.1\n\
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

pub fn sanitize(name: &str) -> String {
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
    Ok(home_dir(Kind::Prefabs)?.join(autosave_file("session")))
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

/// What the history journal holds on disk, so the next autosave can append
/// only the nodes recorded since (see [`save_session_journaled`]).
#[derive(Debug, Default, Clone)]
pub struct JournalState {
    /// Per journal file: nodes already in it, and (parent, label, version)
    /// of the last one, to tell that the live history still extends the
    /// file (not pruned or swapped).
    files: std::collections::HashMap<PathBuf, (usize, Option<(Option<usize>, String, u64)>)>,
}

/// One history node as a journal line; children are rebuilt from parents.
#[derive(Serialize, Deserialize)]
struct JournalLine {
    model: Model,
    #[serde(default)]
    parent: Option<usize>,
    #[serde(default)]
    label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    author: Option<String>,
}

/// The history journal of the document named `doc` (each open document
/// has its own, so switching documents appends instead of rewriting).
fn journal_path_for(doc: &str) -> Result<PathBuf, ModelerError> {
    let key = sanitize(doc);
    let key = if key.is_empty() {
        "untitled".to_string()
    } else {
        key
    };
    let name = autosave_file("session").replace(".json", &format!(".history.{key}.jsonl"));
    Ok(home_dir(Kind::Prefabs)?.join(name))
}

fn node_key(n: &crate::Snapshot) -> (Option<usize>, String, u64) {
    (n.parent, n.label.clone(), n.model.version)
}

/// Autosaves the session with its history in an append-only journal
/// (`session.history.jsonl`, one node per line): only the nodes recorded
/// since the last save are written, instead of the whole tree every time.
/// `session.json` holds the model with an empty node list (and the cursor).
/// When the history no longer extends what was written (pruned, another
/// document loaded), the journal is rewritten whole. Op-derived parts are
/// left out as in [`save_session_state`].
pub fn save_session_journaled(
    model: &mut Model,
    st: &mut JournalState,
) -> Result<(), ModelerError> {
    ensure_project_gitignore();
    let path = session_state_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| ModelerError::Io(format!("creating {}: {e}", parent.display())))?;
    }
    append_journal(model, st)?;
    // The model itself, its node list moved out (the journal has it).
    let taken = std::mem::take(&mut model.history.nodes);
    let text = serde_json::to_string(&slim(model).as_ref())
        .map_err(|e| ModelerError::Parse(e.to_string()));
    model.history.nodes = taken;
    atomic_write(&path, text?.as_bytes())
}

/// Brings `model`'s journal up to date: appends the nodes recorded since
/// the last write, or rewrites it when the history no longer extends it.
/// A journal this process hasn't written yet (after a restart) is checked
/// by its line count and last line, so it is appended to, not rewritten.
fn append_journal(model: &Model, st: &mut JournalState) -> Result<(), ModelerError> {
    let jpath = journal_path_for(&model.name)?;
    let nodes = &model.history.nodes;
    let known = match st.files.get(&jpath) {
        Some(k) => Some(k.clone()),
        None => fs::read_to_string(&jpath).ok().map(|t| {
            let n = t.lines().count();
            let tail = t
                .lines()
                .last()
                .and_then(|l| serde_json::from_str::<JournalLine>(l).ok())
                .map(|j| (j.parent, j.label, j.model.version));
            (n, tail)
        }),
    };
    let extends = match &known {
        Some((w, tail)) if *w <= nodes.len() => match (tail, *w) {
            (None, 0) => true,
            (Some(k), w) if w > 0 => node_key(&nodes[w - 1]) == *k,
            _ => false,
        },
        _ => false,
    };
    let line = |n: &crate::Snapshot| {
        serde_json::to_string(&JournalLine {
            model: n.model.for_file(),
            parent: n.parent,
            label: n.label.clone(),
            author: n.author.clone(),
        })
        .map_err(|e| ModelerError::Parse(e.to_string()))
    };
    let written = known.as_ref().map_or(0, |k| k.0);
    if extends && jpath.exists() {
        if written < nodes.len() {
            let mut text = String::new();
            for n in &nodes[written..] {
                text += &line(n)?;
                text.push('\n');
            }
            use std::io::Write;
            let mut f = fs::OpenOptions::new()
                .append(true)
                .open(&jpath)
                .map_err(|e| ModelerError::Io(format!("opening {}: {e}", jpath.display())))?;
            f.write_all(text.as_bytes())
                .map_err(|e| ModelerError::Io(format!("writing {}: {e}", jpath.display())))?;
        }
    } else {
        let mut text = String::new();
        for n in nodes {
            text += &line(n)?;
            text.push('\n');
        }
        atomic_write(&jpath, text.as_bytes())?;
    }
    st.files
        .insert(jpath, (nodes.len(), nodes.last().map(node_key)));
    Ok(())
}

/// The nodes of `doc`'s journal, else (files from before per-document
/// journals) the shared one when it belonged to the session model.
fn journal_nodes(doc: &str, legacy: bool) -> Option<Vec<crate::Snapshot>> {
    if let Ok(p) = journal_path_for(doc)
        && p.exists()
    {
        return read_journal(&p);
    }
    if !legacy {
        return None;
    }
    let dir = home_dir(Kind::Prefabs).ok()?;
    let slot = format!("session.{}.json", autosave_slot());
    let jname = if autosave_owner().is_some() && dir.join(&slot).exists() {
        slot.replace(".json", ".history.jsonl")
    } else {
        "session.history.jsonl".to_string()
    };
    read_journal(&dir.join(jname))
}

/// Reads the journal's nodes (children rebuilt from parents); a torn last
/// line (a crash mid-append) is dropped.
fn read_journal(path: &std::path::Path) -> Option<Vec<crate::Snapshot>> {
    let text = fs::read_to_string(path).ok()?;
    let mut nodes: Vec<crate::Snapshot> = Vec::new();
    for l in text.lines() {
        let Ok(j) = serde_json::from_str::<JournalLine>(l) else {
            break;
        };
        let id = nodes.len();
        let parent = j.parent.filter(|&p| p < id);
        if let Some(p) = parent {
            nodes[p].children.push(id);
        }
        nodes.push(crate::Snapshot {
            model: j.model,
            parent,
            children: Vec::new(),
            label: j.label,
            author: j.author,
        });
    }
    Some(nodes)
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

/// The autosave files this server writes, paired with their `.prev` names:
/// the session, its history journal, the open docs and the working
/// materials.
fn autosave_files() -> Vec<(PathBuf, PathBuf)> {
    let mut out = Vec::new();
    let prev = |p: PathBuf| {
        let n = p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let to = if let Some((stem, rest)) = n.split_once(".history.")
            && n.ends_with(".jsonl")
        {
            format!("{stem}.prev.history.{rest}")
        } else {
            n.trim_end_matches(".json").to_string() + ".prev.json"
        };
        let q = p.with_file_name(to);
        (p, q)
    };
    if let Ok(p) = session_state_path() {
        out.push(prev(p));
    }
    // Every history journal: the per-document ones and the older shared one.
    if let Ok(d) = home_dir(Kind::Prefabs) {
        let stem = autosave_file("session").replace(".json", ".history.");
        if let Ok(rd) = fs::read_dir(&d) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_string();
                if n.starts_with(&stem) && n.ends_with(".jsonl") {
                    out.push(prev(e.path()));
                }
            }
        }
    }
    if let Ok(d) = home_dir(Kind::Prefabs) {
        out.push(prev(d.join(autosave_file("open_docs"))));
        out.push(prev(d.join(autosave_file("parked_docs"))));
    }
    if let Ok(d) = home_dir(Kind::Materials) {
        out.push(prev(d.join(autosave_file("session"))));
    }
    out
}

/// A server starting empty (no `--restore`) moves the last autosave aside
/// to `*.prev.*` instead of letting its first edit overwrite it. Returns
/// how many files were kept.
pub fn keep_previous_session() -> usize {
    autosave_files()
        .into_iter()
        .filter(|(from, to)| {
            fs::metadata(from).is_ok_and(|m| m.len() > 0) && fs::rename(from, to).is_ok()
        })
        .count()
}

/// Before a `--restore`: when there is no current autosave but a kept
/// `*.prev.*` one (an empty-started server made no edits), bring it back.
pub fn reclaim_previous_session() -> usize {
    let files = autosave_files();
    if files
        .iter()
        .any(|(from, _)| fs::metadata(from).is_ok_and(|m| m.len() > 0))
    {
        return 0;
    }
    files
        .into_iter()
        .filter(|(from, to)| to.exists() && fs::rename(to, from).is_ok())
        .count()
}

/// Autosaves any per-kind session state (e.g. the working materials) as
/// `session.json` in that kind's folder.
pub fn save_kind_state<T: Serialize>(kind: Kind, state: &T) -> Result<(), ModelerError> {
    let dir = home_dir(kind)?;
    fs::create_dir_all(&dir)
        .map_err(|e| ModelerError::Io(format!("creating {}: {e}", dir.display())))?;
    let text = serde_json::to_string(state).map_err(|e| ModelerError::Parse(e.to_string()))?;
    atomic_write(&dir.join(autosave_file("session")), text.as_bytes())
}

/// The autosaved per-kind session state, if present and parseable.
pub fn load_kind_state<T: serde::de::DeserializeOwned>(kind: Kind) -> Option<T> {
    let text = read_autosave(&home_dir(kind).ok()?, "session")?;
    serde_json::from_str(&text).ok()
}

/// Autosaves the inactive open documents (`open_docs.json`), so a restart
/// restores every open scene, not just the active one. Their histories go
/// to their own journals (appended, usually nothing new), so the file holds
/// only what changes when documents switch.
pub fn save_open_docs(docs: &mut [Model], st: &mut JournalState) -> Result<(), ModelerError> {
    let dir = home_dir(Kind::Prefabs)?;
    fs::create_dir_all(&dir)
        .map_err(|e| ModelerError::Io(format!("creating {}: {e}", dir.display())))?;
    for d in docs.iter() {
        append_journal(d, st)?;
    }
    let taken: Vec<_> = docs
        .iter_mut()
        .map(|d| std::mem::take(&mut d.history.nodes))
        .collect();
    let text = {
        let slim: Vec<std::borrow::Cow<'_, Model>> = docs.iter().map(|d| slim(d)).collect();
        serde_json::to_string(&slim).map_err(|e| ModelerError::Parse(e.to_string()))
    };
    for (d, n) in docs.iter_mut().zip(taken) {
        d.history.nodes = n;
    }
    atomic_write(&dir.join(autosave_file("open_docs")), text?.as_bytes())
}

/// Autosaves the parked documents (`parked_docs.json`, see
/// `Session::parked`) with their histories inline: a parked document's name
/// may be reopened from the catalog meanwhile, whose journal would not
/// match this history.
pub fn save_parked_docs(docs: &[Model]) -> Result<(), ModelerError> {
    let dir = home_dir(Kind::Prefabs)?;
    fs::create_dir_all(&dir)
        .map_err(|e| ModelerError::Io(format!("creating {}: {e}", dir.display())))?;
    let path = dir.join(autosave_file("parked_docs"));
    if docs.is_empty() {
        let _ = fs::remove_file(&path);
        return Ok(());
    }
    let slim: Vec<Model> = docs
        .iter()
        .map(|d| {
            let mut d = d.clone();
            if d.parts_derive_from_ops() {
                d.parts.clear();
            }
            d
        })
        .collect();
    let text = serde_json::to_string(&slim).map_err(|e| ModelerError::Parse(e.to_string()))?;
    atomic_write(&path, text.as_bytes())
}

/// The parked documents' autosave (empty when missing). A file that
/// doesn't parse, or holds a document from a newer build, is moved aside to
/// `parked_docs.bad.<time>.json` rather than read in part: the next save
/// would otherwise overwrite it and lose every document in it. A slot
/// server (a second daemon) reads only its own slot's file, never the main
/// daemon's.
pub fn load_parked_docs() -> Vec<Model> {
    let Ok(dir) = home_dir(Kind::Prefabs) else {
        return Vec::new();
    };
    let path = dir.join(autosave_file("parked_docs"));
    let Ok(text) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    let docs: Option<Vec<Model>> = serde_json::from_str(&text).ok();
    match docs {
        Some(docs) if docs.iter().all(|m| m.check_format().is_ok()) => docs
            .into_iter()
            .map(|mut m| {
                let _ = m.restore_parts();
                m
            })
            .collect(),
        _ => {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or_default();
            let aside = path.with_file_name(format!("parked_docs.bad.{stamp}.json"));
            match fs::rename(&path, &aside) {
                Ok(()) => eprintln!(
                    "parked documents unreadable by this build; kept as {}",
                    aside.display()
                ),
                Err(e) => {
                    eprintln!("parked documents unreadable, and moving them aside failed: {e}")
                }
            }
            Vec::new()
        }
    }
}

/// The autosaved inactive documents (empty when missing or unreadable).
pub fn load_open_docs() -> Vec<Model> {
    let Ok(dir) = home_dir(Kind::Prefabs) else {
        return Vec::new();
    };
    let Some(text) = read_autosave(&dir, "open_docs") else {
        return Vec::new();
    };
    let docs: Vec<Model> = serde_json::from_str(&text).unwrap_or_default();
    docs.into_iter()
        .filter(|m| m.check_format().is_ok())
        .map(|mut m| {
            if m.history.nodes.is_empty()
                && let Some(nodes) = journal_nodes(&m.name, false)
            {
                m.history.current = m
                    .history
                    .current
                    .filter(|&c| c < nodes.len())
                    .or_else(|| nodes.len().checked_sub(1));
                m.history.nodes = nodes;
            }
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
    Ok(home_dir(Kind::Prefabs)?.join("session.lock"))
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
    let text = read_autosave(&home_dir(Kind::Prefabs).ok()?, "session")?;
    let mut model: Model = serde_json::from_str(&text).ok()?;
    if let Err(e) = model.check_format() {
        eprintln!("ignoring session autosave: {e}");
        return None;
    }
    if model.history.nodes.is_empty()
        && let Some(nodes) = journal_nodes(&model.name, true)
    {
        model.history.current = model
            .history
            .current
            .filter(|&c| c < nodes.len())
            .or_else(|| nodes.len().checked_sub(1));
        model.history.nodes = nodes;
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
    // The model without its history: the undo tree goes to a git-ignored
    // sidecar (see `history_path`), so the tracked file holds only what
    // rebuilds the model.
    let mut saved = model.clone_without_history();
    saved.name = name.clone();
    if saved.parts_derive_from_ops() {
        saved.parts.clear();
    }
    let dir = catalog_dir()?;
    fs::create_dir_all(&dir)
        .map_err(|e| ModelerError::Io(format!("creating {}: {e}", dir.display())))?;
    ensure_project_gitignore();
    let file = format!("{name}.json");
    let text =
        serde_json::to_string_pretty(&saved).map_err(|e| ModelerError::Parse(e.to_string()))?;
    let path = dir.join(&file);
    write_history_sidecar(&dir, &name, &model.history)?;
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
    let side = history_path(&dir, &e.name);
    if let Some(f) = side.file_name() {
        let _ = fs::rename(&side, bin.join(f));
    }
    save_index(kind, &catalog)
}

/// Load a saved model by name. Procedural prefabs store no parts; they are
/// re-derived from the ops here, so a graph prefab is small on disk and still
/// opens fully evaluated.
pub fn load_prefab(name: &str) -> Result<Model, ModelerError> {
    if let Some(m) = open_doc(name) {
        return Ok((*m).clone());
    }
    load_prefab_file(name)
}

static OPEN_DOCS: std::sync::Mutex<
    Option<std::collections::HashMap<String, std::sync::Arc<Model>>>,
> = std::sync::Mutex::new(None);

/// Registers (or with `None` removes) a document open in the session, so
/// prefab instances of that name resolve to the live doc before the file.
pub fn set_open_doc(name: &str, model: Option<Model>) {
    if let Ok(mut g) = OPEN_DOCS.lock() {
        let map = g.get_or_insert_with(Default::default);
        match model {
            Some(m) => {
                map.insert(name.to_string(), std::sync::Arc::new(m));
            }
            None => {
                map.remove(name);
            }
        }
    }
}

/// The open document named `name`, if any (shared, not copied).
pub fn open_doc(name: &str) -> Option<std::sync::Arc<Model>> {
    OPEN_DOCS
        .lock()
        .ok()
        .and_then(|g| g.as_ref().and_then(|m| m.get(name).cloned()))
}

type InstanceCache =
    std::collections::HashMap<PathBuf, (Option<std::time::SystemTime>, u64, std::sync::Arc<Model>)>;
static INSTANCES: std::sync::Mutex<Option<InstanceCache>> = std::sync::Mutex::new(None);

/// A prefab for reading, not editing: the open document of that name, else
/// the catalog file derived without its undo history, cached by path and
/// modification time (instancing, markers, lights and scene export call this
/// for every instance on every evaluation).
pub fn prefab_instance(name: &str) -> Result<std::sync::Arc<Model>, ModelerError> {
    if let Some(m) = open_doc(name) {
        return Ok(m);
    }
    let dir = catalog_dir()?;
    let path = dir.join(format!("{}.json", sanitize(name)));
    let Ok(meta) = fs::metadata(&path) else {
        return Err(ModelerError::Io(missing_prefab(name)));
    };
    let stamp = (meta.modified().ok(), meta.len());
    if let Ok(g) = INSTANCES.lock()
        && let Some((t, l, m)) = g.as_ref().and_then(|c| c.get(&path))
        && (*t, *l) == stamp
    {
        return Ok(m.clone());
    }
    let text = fs::read_to_string(&path)
        .map_err(|e| ModelerError::Io(format!("reading {}: {e}", path.display())))?;
    let mut model: Model = serde_json::from_str(&text)
        .map_err(|e| ModelerError::Parse(format!("parsing {}: {e}", path.display())))?;
    model.check_format()?;
    model.history = Default::default();
    model.restore_parts()?;
    let model = std::sync::Arc::new(model);
    if let Ok(mut g) = INSTANCES.lock() {
        g.get_or_insert_with(Default::default)
            .insert(path, (stamp.0, stamp.1, model.clone()));
    }
    Ok(model)
}

/// "no prefab `x`", with the closest catalog names (an entry the name
/// starts with, e.g. a label like "de-lamp lamp 4", or one sharing a word).
fn missing_prefab(name: &str) -> String {
    let want = sanitize(name);
    let first = sanitize(name.split_whitespace().next().unwrap_or(name));
    let names: Vec<String> = load_catalog()
        .map(|c| c.entries.into_iter().map(|e| e.name).collect())
        .unwrap_or_default();
    let mut close: Vec<&String> = names
        .iter()
        .filter(|n| {
            **n == first
                || want.starts_with(n.as_str())
                || n.contains(&want)
                || want
                    .split('-')
                    .filter(|w| w.len() > 3)
                    .any(|w| n.contains(w))
        })
        .collect();
    close.sort_by_key(|n| (**n != first, !want.starts_with(n.as_str()), n.len()));
    close.truncate(5);
    if close.is_empty() {
        format!("no prefab `{name}` in the catalog (catalog_list shows them)")
    } else {
        let list: Vec<String> = close.iter().map(|n| format!("`{n}`")).collect();
        format!(
            "no prefab `{name}` in the catalog; did you mean {}? (load it by its exact name; catalog_list shows them)",
            list.join(", ")
        )
    }
}

/// A fingerprint of the catalog file of prefab `name` (None: no file), so
/// a save can tell whether someone else wrote it since it was loaded.
pub fn prefab_file_hash(name: &str) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let path = catalog_dir().ok()?.join(format!("{}.json", sanitize(name)));
    let bytes = fs::read(path).ok()?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    Some(h.finish())
}

/// [`load_prefab`] from the catalog file only, ignoring open documents.
/// Whether a model's ops and params equal its catalog file's (read as plain
/// JSON, nothing derived): closing it loses nothing.
pub fn matches_prefab_file(model: &Model) -> bool {
    let Ok(dir) = catalog_dir() else {
        return false;
    };
    let path = dir.join(format!("{}.json", sanitize(&model.name)));
    let Some(file) = fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
    else {
        return false;
    };
    let same = |key: &str, v: serde_json::Result<serde_json::Value>| {
        v.ok().is_some_and(|v| {
            file.get(key).unwrap_or(&serde_json::Value::Null) == &v
                || (v.as_object().is_some_and(|o| o.is_empty()) && file.get(key).is_none())
        })
    };
    same("ops", serde_json::to_value(&model.ops))
        && same("params", serde_json::to_value(&model.params))
}

/// A prefab's undo tree, beside it: `<name>.history.json` (git-ignored in a
/// project; per machine, like the session autosave).
pub fn history_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{}.history.json", sanitize(name)))
}

/// Writes (or, for an empty history, removes) a prefab's history sidecar.
/// Compact: snapshots repeat most of the model, so pretty printing would
/// multiply the size.
fn write_history_sidecar(
    dir: &Path,
    name: &str,
    history: &crate::History,
) -> Result<(), ModelerError> {
    let path = history_path(dir, name);
    if history.nodes.is_empty() {
        let _ = fs::remove_file(&path);
        return Ok(());
    }
    let text = serde_json::to_string(history).map_err(|e| ModelerError::Parse(e.to_string()))?;
    atomic_write(&path, text.as_bytes())
}

/// Moves the history out of every prefab file in the catalog that still
/// holds it inline (files saved before the sidecar), leaving the model
/// itself unchanged. Returns (files rewritten, bytes before, bytes after).
pub fn split_prefab_histories() -> Result<(usize, u64, u64), ModelerError> {
    ensure_project_gitignore();
    let dir = catalog_dir()?;
    let (mut n, mut before, mut after) = (0, 0u64, 0u64);
    for e in load_catalog()?.entries {
        let path = dir.join(&e.file);
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let Some(obj) = v.as_object_mut() else {
            continue;
        };
        let Some(hist) = obj.remove("history") else {
            continue;
        };
        let has_nodes = hist
            .get("nodes")
            .and_then(|n| n.as_array())
            .is_some_and(|a| !a.is_empty());
        if has_nodes {
            let h = serde_json::to_string(&hist).map_err(|e| ModelerError::Parse(e.to_string()))?;
            atomic_write(&history_path(&dir, &e.name), h.as_bytes())?;
        }
        let out =
            serde_json::to_string_pretty(&v).map_err(|e| ModelerError::Parse(e.to_string()))?;
        atomic_write(&path, out.as_bytes())?;
        before += text.len() as u64;
        after += out.len() as u64;
        n += 1;
    }
    Ok((n, before, after))
}

pub fn load_prefab_file(name: &str) -> Result<Model, ModelerError> {
    let dir = catalog_dir()?;
    let path = dir.join(format!("{}.json", sanitize(name)));
    if !path.exists() {
        return Err(ModelerError::Io(missing_prefab(name)));
    }
    let text = fs::read_to_string(&path)
        .map_err(|e| ModelerError::Io(format!("reading {}: {e}", path.display())))?;
    let mut model: Model = serde_json::from_str(&text)
        .map_err(|e| ModelerError::Parse(format!("parsing {}: {e}", path.display())))?;
    model.check_format()?;
    // The history from its sidecar (files saved before it hold it inline).
    if model.history.nodes.is_empty()
        && let Ok(t) = fs::read_to_string(history_path(&dir, name))
        && let Ok(h) = serde_json::from_str::<crate::History>(&t)
    {
        model.history = h;
    }
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

    /// The undo tree goes to a sidecar beside the prefab file and comes back
    /// on load; old files with inline history split out unchanged.
    #[test]
    fn prefab_history_lives_in_a_sidecar() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let root = temp_root("sidecar");
        // SAFETY: test-only, under ENV_LOCK.
        unsafe { std::env::set_var("FRIENDSLOP_DATA_DIR", &root) };
        let ok = |r: crate::commands::CmdResult| r.unwrap_or_else(|e| panic!("{e}"));
        let mut s = crate::commands::Session::default();
        s.model.name = "crate".into();
        for size in [1.0, 1.5, 2.0] {
            ok(s.apply_values(
                None,
                vec![serde_json::json!({"type": "primitive", "name": "b", "kind": "box", "size": size})],
            ));
        }
        let nodes = s.model.history.nodes.len();
        assert!(nodes >= 3);
        save_prefab(&s.model, "d", &[], "p").unwrap_or_else(|e| panic!("{e}"));
        let dir = catalog_dir().unwrap_or_else(|e| panic!("{e}"));
        let main: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(dir.join("crate.json")).unwrap_or_else(|e| panic!("{e}")),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let inline = main["history"]["nodes"].as_array().map_or(0, |a| a.len());
        assert_eq!(inline, 0, "the tracked file holds no history");
        assert!(history_path(&dir, "crate").exists());
        let back = load_prefab_file("crate").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(back.history.nodes.len(), nodes);
        assert_eq!(back.history.current, s.model.history.current);
        // An old-style file: history inline, no sidecar.
        let mut old = s.model.clone();
        old.name = "crate".into();
        old.parts.clear();
        let _ = fs::remove_file(history_path(&dir, "crate"));
        fs::write(
            dir.join("crate.json"),
            serde_json::to_string_pretty(&old).unwrap_or_else(|e| panic!("{e}")),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            load_prefab_file("crate")
                .map(|m| m.history.nodes.len())
                .ok(),
            Some(nodes),
            "inline history still loads"
        );
        let (n, before, after) = split_prefab_histories().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(n, 1);
        assert!(after < before, "{before} -> {after}");
        let split = load_prefab_file("crate").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(split.history.nodes.len(), nodes);
        assert_eq!(split.ops.len(), s.model.ops.len());
        // Deleting the entry takes the sidecar along.
        delete_doc(Kind::Prefabs, "crate").unwrap_or_else(|e| panic!("{e}"));
        assert!(!history_path(&dir, "crate").exists());
        assert!(dir.join("removed").join("crate.history.json").exists());
    }

    /// Idle documents close only when saved and unchanged since; restored
    /// ones are compared with their catalog file.
    #[test]
    fn idle_saved_documents_close_and_unsaved_ones_stay() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: test-only, under ENV_LOCK.
        unsafe { std::env::set_var("FRIENDSLOP_DATA_DIR", temp_root("idle")) };
        let ok = |r: crate::commands::CmdResult| r.unwrap_or_else(|e| panic!("{e}"));
        let box_ = |n: &str| {
            vec![serde_json::json!({"type": "primitive", "name": n, "kind": "box", "size": 1})]
        };
        let mut s = crate::commands::Session::default();
        for name in ["saved", "edited", "draft"] {
            ok(s.scene("open", Some(name)));
            ok(s.apply_values(None, box_("b")));
            if name != "draft" {
                s.save_prefab_checked("x", &[], "x", false)
                    .unwrap_or_else(|e| panic!("{e}"));
            }
        }
        ok(s.scene("switch", Some("edited")));
        ok(s.apply_values(None, box_("c")));
        ok(s.scene("open", Some("other")));
        // A restored copy of `saved`: no saved state, but equal to its file.
        let restored = s.others.iter().find(|m| m.name == "saved").cloned();
        let closed = s.close_idle(std::time::Duration::ZERO);
        assert_eq!(closed, vec!["saved".to_string()]);
        let mut fresh = crate::commands::Session::default();
        fresh.others.extend(restored);
        assert_eq!(
            fresh.close_idle(std::time::Duration::ZERO),
            vec!["saved".to_string()]
        );
        // Reopening it comes back from the catalog.
        ok(s.scene("open", Some("saved")));
        assert!(s.model.part("b").is_some());
    }

    /// A restart brings back only unsaved documents; an active document that
    /// matches its file leaves an unsaved one (or an empty one) in front.
    #[test]
    fn a_restart_reopens_only_unsaved_documents() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: test-only, under ENV_LOCK.
        unsafe { std::env::set_var("FRIENDSLOP_DATA_DIR", temp_root("restart")) };
        let ok = |r: crate::commands::CmdResult| r.unwrap_or_else(|e| panic!("{e}"));
        let box_ = |n: &str| {
            vec![serde_json::json!({"type": "primitive", "name": n, "kind": "box", "size": 1})]
        };
        let mut s = crate::commands::Session::default();
        for name in ["saved", "draft", "front"] {
            ok(s.scene("open", Some(name)));
            ok(s.apply_values(None, box_("b")));
            if name != "draft" {
                s.save_prefab_checked("x", &[], "x", false)
                    .unwrap_or_else(|e| panic!("{e}"));
            }
        }
        // What a restore hands over: the active document and the others.
        let mut fresh = crate::commands::Session::default();
        fresh.model = s.model.clone();
        fresh.others = s.others.clone();
        let mut closed = fresh.close_saved_documents();
        closed.sort();
        assert_eq!(closed, ["front", "saved"]);
        assert_eq!(fresh.model.name, "draft");
        assert!(fresh.others.is_empty());
    }

    /// A restart parks unsaved documents instead of opening them; an agent
    /// brings one back with `scene restore`, which survives more restarts
    /// until then.
    #[test]
    fn a_restart_parks_unsaved_documents_until_restored() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: test-only, under ENV_LOCK.
        unsafe { std::env::set_var("FRIENDSLOP_DATA_DIR", temp_root("park")) };
        let ok = |r: crate::commands::CmdResult| r.unwrap_or_else(|e| panic!("{e}"));
        let mut s = crate::commands::Session::default();
        ok(s.scene("open", Some("draft")));
        ok(s.apply_values(
            None,
            vec![serde_json::json!({"type": "primitive", "name": "b", "kind": "box", "size": 1})],
        ));
        let mut fresh = crate::commands::Session::default();
        fresh.model = s.model.clone();
        fresh.others = s.others.clone();
        fresh.close_saved_documents();
        assert_eq!(fresh.park_restored(), ["draft"]);
        assert_eq!(fresh.model.name, "model");
        assert!(fresh.others.is_empty());
        // Another restart before anyone asked: still parked.
        let mut again = crate::commands::Session::default();
        assert_eq!(again.park_restored(), Vec::<String>::new());
        assert_eq!(again.parked.len(), 1);
        let said = ok(again.scene("restore", Some("draft")));
        assert!(said.contains("restored"), "{said}");
        assert_eq!(again.model.name, "draft");
        assert_eq!(again.model.ops.len(), 1);
        assert!(again.parked.is_empty());
        assert!(load_parked_docs().is_empty());
    }

    /// Calls name their project: the catalog follows it, and a document of
    /// the same name from another project is not taken over.
    #[test]
    fn each_call_works_in_its_callers_project() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: test-only, under ENV_LOCK.
        unsafe { std::env::remove_var("FRIENDSLOP_DATA_DIR") };
        let root = temp_root("projects");
        let (a, b) = (root.join("game_a"), root.join("game_b"));
        for p in [&a, &b] {
            fs::create_dir_all(p.join(PROJECT_DIR)).unwrap_or_else(|e| panic!("{e}"));
        }
        let ok = |r: crate::commands::CmdResult| r.unwrap_or_else(|e| panic!("{e}"));
        let mut s = crate::commands::Session::default();
        s.begin_call(Some("A"), None, Some(Some(a.clone())));
        assert_eq!(project_root(), Some(a.clone()));
        ok(s.scene("open", Some("crate")));
        ok(s.apply_values(
            None,
            vec![serde_json::json!({"type": "primitive", "name": "b", "kind": "box", "size": 1})],
        ));
        s.save_prefab_checked("x", &[], "x", false)
            .unwrap_or_else(|e| panic!("{e}"));
        s.end_call(Some("A"));
        assert_eq!(s.model.project, Some(a.clone()));
        assert!(a.join(".modeler/prefabs/crate.json").is_file());
        assert!(!b.join(".modeler/prefabs/crate.json").exists());
        assert_eq!(call_project(), None);
        ok(s.scene("open", Some("other")));
        s.begin_call(Some("B"), None, Some(Some(b.clone())));
        let clash = s.scene("open", Some("crate"));
        assert!(
            clash
                .as_ref()
                .is_err_and(|e| e.to_string().contains("game_a")),
            "{clash:?}"
        );
        s.end_call(Some("B"));
        // B starting a model while A's document is in front keeps A's open.
        s.begin_call(Some("A"), None, Some(Some(a.clone())));
        assert_eq!(s.model.name, "crate");
        s.end_call(Some("A"));
        s.begin_call(Some("B"), None, Some(Some(b.clone())));
        ok(s.new_model("b_doc"));
        assert!(s.others.iter().any(|m| m.name == "crate"));
        s.end_call(Some("B"));
        assert_eq!(s.model.project, Some(b.clone()));
        let _ = fs::remove_dir_all(&root);
    }

    /// Two agents sharing one server: each keeps its document, edits are
    /// credited, undo won't revert the other's step unasked, and a prefab
    /// file another server wrote is not silently overwritten.
    #[test]
    fn agents_sharing_a_server_keep_their_docs_and_work() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: test-only, under ENV_LOCK.
        unsafe { std::env::set_var("FRIENDSLOP_DATA_DIR", temp_root("agents")) };
        let ok = |r: crate::commands::CmdResult| r.unwrap_or_else(|e| panic!("{e}"));
        let box_ = |n: &str, size: f32| {
            vec![serde_json::json!({"type": "primitive", "name": n, "kind": "box", "size": size})]
        };
        let mut s = crate::commands::Session::default();
        // A opens its doc and builds.
        s.begin_call(Some("A"), None, None);
        ok(s.scene("open", Some("shed")));
        ok(s.apply_values(None, box_("wall", 1.0)));
        ok(s.save_prefab_checked("d", &[], "p", false).map(|r| r.0));
        s.end_call(Some("A"));
        // B works elsewhere.
        s.begin_call(Some("B"), None, None);
        ok(s.scene("open", Some("cart")));
        ok(s.apply_values(None, box_("bed", 1.0)));
        s.end_call(Some("B"));
        // A is put back on its own doc.
        let note = s.begin_call(Some("A"), None, None).unwrap_or_default();
        assert_eq!(s.model.name, "shed");
        assert!(note.contains("switched back to your `shed`"), "{note}");
        s.end_call(Some("A"));
        // B edits A's doc (pinned), replacing A's op: warned.
        s.begin_call(Some("B"), Some("shed"), None);
        let out = ok(s.apply_values(None, box_("wall", 2.0)));
        assert!(
            out.contains("another agent wrote") && out.contains("(by A)"),
            "{out}"
        );
        s.end_call(Some("B"));
        // A hears about it, and its undo is refused until forced.
        let note = s.begin_call(Some("A"), None, None).unwrap_or_default();
        assert!(note.contains("B made 1 edit"), "{note}");
        let e = s.undo_checked(false).map_err(|e| e.to_string());
        assert!(e.is_err_and(|e| e.contains("another agent (B)")));
        assert!(s.history(None).contains("by B"));
        ok(s.undo_checked(true));
        // Another server rewrote the prefab file: A's save is refused.
        let mut other = s.model.clone();
        other.name = "shed".into();
        other.ops.clear();
        save_prefab(&other, "theirs", &[], "x").unwrap_or_else(|e| panic!("{e}"));
        let e = s
            .save_prefab_checked("d", &[], "p", false)
            .map_err(|e| e.to_string());
        assert!(e.is_err_and(|e| e.contains("changed since you loaded")));
        ok(s.save_prefab_checked("d", &[], "p", true).map(|r| r.0));
        s.end_call(Some("A"));
    }

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
    fn an_empty_start_keeps_the_previous_autosave() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: test-only, under ENV_LOCK.
        unsafe { std::env::set_var("FRIENDSLOP_DATA_DIR", temp_root("prev")) };
        let mut m = Model {
            name: "arena".into(),
            ..Default::default()
        };
        let mut st = JournalState::default();
        save_session_journaled(&mut m, &mut st).unwrap_or_else(|e| panic!("{e}"));
        let session = session_state_path().unwrap_or_else(|e| panic!("{e}"));
        assert!(session.exists());
        // An empty start moves it aside...
        assert!(keep_previous_session() >= 1);
        assert!(!session.exists());
        assert!(session.with_file_name("session.prev.json").exists());
        // ...and a later --restore, with nothing newer written, reclaims it.
        assert!(reclaim_previous_session() >= 1);
        assert_eq!(load_session_state().map(|m| m.name), Some("arena".into()));
        // A newer autosave is never replaced by the kept one.
        assert!(keep_previous_session() >= 1);
        let mut n = Model {
            name: "newer".into(),
            ..Default::default()
        };
        save_session_journaled(&mut n, &mut JournalState::default())
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(reclaim_previous_session(), 0);
        assert_eq!(load_session_state().map(|m| m.name), Some("newer".into()));
    }

    #[test]
    fn open_docs_keep_their_history_in_their_own_journals() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: test-only, under ENV_LOCK.
        unsafe { std::env::set_var("FRIENDSLOP_DATA_DIR", temp_root("docjournal")) };
        let mut s = crate::commands::Session::default();
        let box_at = |x: i32| -> Vec<serde_json::Value> {
            serde_json::from_str(&format!(
                r#"[{{"type":"primitive","name":"b","kind":"box","size":0.1,"translation":[{x},0.05,0]}}]"#
            ))
            .unwrap_or_else(|e| panic!("{e}"))
        };
        s.new_model("doc_a").unwrap_or_else(|e| panic!("{e}"));
        for x in 0..3 {
            s.apply_values(None, box_at(x))
                .unwrap_or_else(|e| panic!("{e}"));
        }
        s.scene("open", Some("doc_b"))
            .unwrap_or_else(|e| panic!("{e}"));
        s.apply_values(None, box_at(9))
            .unwrap_or_else(|e| panic!("{e}"));
        let a_nodes = s
            .others
            .iter()
            .find(|m| m.name == "doc_a")
            .map(|m| m.history.nodes.len())
            .unwrap_or_else(|| panic!("doc_a not open"));
        s.mark_autosave(true, true, false);
        s.flush_autosave(true);
        let dir = home_dir(Kind::Prefabs).unwrap_or_else(|e| panic!("{e}"));
        let raw = fs::read_to_string(dir.join("open_docs.json")).unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
        assert!(
            v.as_array()
                .is_some_and(|d| d.iter().all(|m| m["history"]["nodes"]
                    .as_array()
                    .is_none_or(|n| n.is_empty()))),
            "open_docs.json carries no snapshots"
        );
        let back = load_open_docs();
        let a = back
            .iter()
            .find(|m| m.name == "doc_a")
            .unwrap_or_else(|| panic!("doc_a not restored"));
        assert_eq!(a.history.nodes.len(), a_nodes);
        assert_eq!(a.parts.len(), 1);
    }

    #[test]
    fn session_history_goes_to_an_append_only_journal() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: test-only, single-threaded; edition 2024 makes set_var unsafe.
        unsafe { std::env::set_var("FRIENDSLOP_DATA_DIR", temp_root("journal")) };
        let mut s = crate::commands::Session::default();
        let step = |s: &mut crate::commands::Session, i: usize| {
            let item: Vec<serde_json::Value> = serde_json::from_str(&format!(
                r#"[{{"type":"primitive","name":"p{i}","kind":"box","size":0.1,"translation":[{i},0.05,0]}}]"#
            ))
            .unwrap_or_else(|e| panic!("{e}"));
            s.apply_values(None, item).unwrap_or_else(|e| panic!("{e}"));
        };
        let jpath = journal_path_for(&s.model.name).unwrap_or_else(|e| panic!("{e}"));
        let lines = || {
            fs::read_to_string(&jpath)
                .unwrap_or_default()
                .lines()
                .count()
        };
        let mut st = JournalState::default();
        for i in 0..3 {
            step(&mut s, i);
        }
        save_session_journaled(&mut s.model, &mut st).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(lines(), 3);
        let raw = fs::read_to_string(session_state_path().unwrap_or_else(|e| panic!("{e}")))
            .unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
        assert!(
            v["history"]["nodes"]
                .as_array()
                .is_none_or(|n| n.is_empty()),
            "session.json carries no snapshots"
        );
        let before = fs::read_to_string(&jpath).unwrap_or_default();
        for i in 3..5 {
            step(&mut s, i);
        }
        save_session_journaled(&mut s.model, &mut st).unwrap_or_else(|e| panic!("{e}"));
        let after = fs::read_to_string(&jpath).unwrap_or_default();
        assert_eq!(lines(), 5);
        assert!(after.starts_with(&before), "appended, not rewritten");
        let back = load_session_state().unwrap_or_else(|| panic!("no autosave"));
        assert_eq!(back.history.nodes.len(), 5);
        assert_eq!(back.history.current, s.model.history.current);
        let kids = |m: &Model| {
            m.history
                .nodes
                .iter()
                .map(|n| n.children.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(kids(&back), kids(&s.model));
        assert_eq!(back.parts.len(), 5, "parts re-derive from the ops");
        // A prune no longer extends the file: it is rewritten.
        s.history_prune(2).unwrap_or_else(|e| panic!("{e}"));
        save_session_journaled(&mut s.model, &mut st).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(lines(), s.model.history.nodes.len());
        let back = load_session_state().unwrap_or_else(|| panic!("no autosave"));
        assert_eq!(back.history.nodes.len(), s.model.history.nodes.len());
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
