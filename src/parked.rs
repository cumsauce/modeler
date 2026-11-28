//! Parked documents: unsaved work a restarted server found in its autosave,
//! kept on disk but not open (`Session::parked`, `scene restore`).
//!
//! Each document is its own file, `parked/<project>/<stem>.jsonl` beside the
//! prefabs (`parked.<slot>/` for a slot server): line 1 is its index entry
//! ([`ParkedDoc`]: name, project, op count, version, last editor, when it was
//! parked), line 2 the model. Startup reads only the first lines; a body is
//! read when an agent restores it. Parking or dropping one document writes
//! or removes only its own file. The older single `parked_docs.json` (every
//! document with its whole history inline: 866 MB once, parsed and
//! re-derived at every start) is migrated on first start and kept in
//! `archive/`.

use std::fs;
use std::io::BufRead;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::catalog::{self, Kind};
use crate::{Model, ModelerError};

/// One parked document's index entry: the first line of its file.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ParkedDoc {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<PathBuf>,
    /// Ops in the document.
    #[serde(default)]
    pub ops: usize,
    #[serde(default)]
    pub version: u64,
    /// Who made the edit at the history cursor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// When it was parked (Unix seconds).
    #[serde(default)]
    pub parked_at: u64,
    /// History nodes kept in the file.
    #[serde(default)]
    pub history: usize,
    /// The file holding it (not stored: where it was read from or written).
    #[serde(skip)]
    pub file: PathBuf,
}

/// FNV-1a: a hash that stays the same across builds (file names).
pub(crate) fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

fn mtime_secs(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// This server's parked folder: `parked/`, or `parked.<slot>/` while
/// another server owns the main autosave.
pub fn root() -> Result<PathBuf, ModelerError> {
    let name = catalog::autosave_file("parked");
    let name = name.trim_end_matches(".json");
    Ok(catalog::home_dir(Kind::Prefabs)?.join(name))
}

/// The folder of one project's parked documents: `library` for the shared
/// library, else the project folder's name and a hash of its path.
pub fn project_dir_name(project: Option<&Path>) -> String {
    match project {
        None => "library".to_string(),
        Some(p) => {
            let base = p
                .file_name()
                .map(|n| catalog::sanitize(&n.to_string_lossy()))
                .unwrap_or_else(|| "project".into());
            let h = fnv(p.to_string_lossy().as_bytes());
            format!("{base}-{:08x}", h & 0xffff_ffff)
        }
    }
}

/// A document's file stem: its sanitized name, with a hash of the exact
/// name when sanitizing changed it (so `a b` and `a-b` never share a file).
fn stem(name: &str) -> String {
    let s = catalog::sanitize(name);
    if s == name {
        s
    } else {
        format!("{s}-{:08x}", fnv(name.as_bytes()) & 0xffff_ffff)
    }
}

/// Where `name` of `project` is parked under `root`.
fn file_for(root: &Path, name: &str, project: Option<&Path>) -> PathBuf {
    root.join(project_dir_name(project))
        .join(format!("{}.jsonl", stem(name)))
}

/// Parks `model`: writes its file (op-derived parts left out, as in every
/// autosave) and returns its entry. `parked_at` defaults to now.
pub fn park(model: &mut Model, parked_at: Option<u64>) -> Result<ParkedDoc, ModelerError> {
    let root = root()?;
    park_in(&root, model, parked_at)
}

fn park_in(
    root: &Path,
    model: &mut Model,
    parked_at: Option<u64>,
) -> Result<ParkedDoc, ModelerError> {
    if model.parts_derive_from_ops() {
        model.parts.clear();
    }
    let file = file_for(root, &model.name, model.project.as_deref());
    let entry = ParkedDoc {
        name: model.name.clone(),
        project: model.project.clone(),
        ops: model.ops.len(),
        version: model.version,
        author: model
            .history
            .current
            .and_then(|c| model.history.nodes.get(c))
            .and_then(|n| n.author.clone()),
        parked_at: parked_at.unwrap_or_else(now_secs),
        history: model.history.nodes.len(),
        file: file.clone(),
    };
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir)
            .map_err(|e| ModelerError::Io(format!("creating {}: {e}", dir.display())))?;
    }
    let mut text = serde_json::to_string(&entry).map_err(|e| ModelerError::Parse(e.to_string()))?;
    text.push('\n');
    text += &serde_json::to_string(&*model).map_err(|e| ModelerError::Parse(e.to_string()))?;
    text.push('\n');
    catalog::atomic_write(&file, text.as_bytes())?;
    Ok(entry)
}

/// A parked file's index entry (its first line only).
pub fn read_entry(path: &Path) -> Option<ParkedDoc> {
    let f = fs::File::open(path).ok()?;
    let mut line = String::new();
    std::io::BufReader::new(f).read_line(&mut line).ok()?;
    let mut e: ParkedDoc = serde_json::from_str(line.trim_end()).ok()?;
    e.file = path.to_path_buf();
    Some(e)
}

/// The parked documents' entries, read from the first line of each file:
/// (parked, archived), oldest first. Migrates an old `parked_docs.json`
/// first.
pub fn load_index() -> (Vec<ParkedDoc>, Vec<ParkedDoc>) {
    migrate_legacy();
    let Ok(root) = root() else {
        return Default::default();
    };
    let mut parked = Vec::new();
    let mut archived = Vec::new();
    let Ok(projects) = fs::read_dir(&root) else {
        return Default::default();
    };
    for p in projects.flatten() {
        if !p.path().is_dir() {
            continue;
        }
        scan(&p.path(), &mut parked);
        scan(&p.path().join("archive"), &mut archived);
    }
    let order = |v: &mut Vec<ParkedDoc>| {
        v.sort_by(|a, b| (a.parked_at, &a.name).cmp(&(b.parked_at, &b.name)));
    };
    order(&mut parked);
    order(&mut archived);
    (parked, archived)
}

fn scan(dir: &Path, out: &mut Vec<ParkedDoc>) {
    let Ok(rd) = fs::read_dir(dir) else {
        return;
    };
    for f in rd.flatten() {
        let path = f.path();
        if path.extension().is_none_or(|x| x != "jsonl") {
            continue;
        }
        match read_entry(&path) {
            Some(e) => out.push(e),
            None => eprintln!("parked document unreadable, left as is: {}", path.display()),
        }
    }
}

/// Reads a parked document's body and re-derives its parts.
pub fn load_body(entry: &ParkedDoc) -> Result<Model, ModelerError> {
    let f = fs::File::open(&entry.file)
        .map_err(|e| ModelerError::Io(format!("reading {}: {e}", entry.file.display())))?;
    let mut r = std::io::BufReader::new(f);
    let mut head = String::new();
    r.read_line(&mut head)
        .map_err(|e| ModelerError::Io(format!("reading {}: {e}", entry.file.display())))?;
    let mut model: Model = serde_json::from_reader(r)
        .map_err(|e| ModelerError::Parse(format!("parsing {}: {e}", entry.file.display())))?;
    model.check_format()?;
    model.restore_parts()?;
    Ok(model)
}

/// Removes a parked document's file (it was restored or dropped).
pub fn remove(entry: &ParkedDoc) {
    if let Err(e) = fs::remove_file(&entry.file)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("removing parked {}: {e}", entry.file.display());
    }
}

/// `<home>/archive/`: where files this server moves aside go (never deleted).
pub fn archive_dir() -> Result<PathBuf, ModelerError> {
    Ok(catalog::home_dir(Kind::Prefabs)?.join("archive"))
}

/// Moves `from` into `dir` (created), keeping its name, or with a time
/// stamp when the name is taken.
pub(crate) fn move_into(from: &Path, dir: &Path) -> Result<PathBuf, ModelerError> {
    fs::create_dir_all(dir)
        .map_err(|e| ModelerError::Io(format!("creating {}: {e}", dir.display())))?;
    let name = from
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".into());
    let mut to = dir.join(&name);
    if to.exists() {
        to = dir.join(format!("{}.{name}", now_secs()));
    }
    fs::rename(from, &to)
        .map_err(|e| ModelerError::Io(format!("moving {}: {e}", from.display())))?;
    Ok(to)
}

/// Each element of a JSON array of models, handed over one at a time, so
/// an old `parked_docs.json` is migrated without holding every document.
struct EachModel<'a>(&'a mut dyn FnMut(Model) -> Result<(), String>);

impl<'de> serde::de::DeserializeSeed<'de> for EachModel<'_> {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_seq(self)
    }
}

impl<'de> serde::de::Visitor<'de> for EachModel<'_> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("an array of models")
    }
    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while let Some(m) = seq.next_element::<Model>()? {
            (self.0)(m).map_err(serde::de::Error::custom)?;
        }
        Ok(())
    }
}

/// First start after the change: the single `parked_docs.json` becomes one
/// file per document (parked at the old file's modification time), and the
/// old file moves to `archive/` whole (with every history node). A file that
/// doesn't parse, or holds a document from a newer build, is moved aside to
/// `parked_docs.bad.<time>.json` as before, and nothing of it is parked.
pub fn migrate_legacy() -> usize {
    let (Ok(home), Ok(root)) = (catalog::home_dir(Kind::Prefabs), root()) else {
        return 0;
    };
    let legacy = home.join(catalog::autosave_file("parked_docs"));
    let Ok(file) = fs::File::open(&legacy) else {
        return 0;
    };
    let when = mtime_secs(&legacy);
    let mut written: Vec<PathBuf> = Vec::new();
    let mut each = |mut m: Model| -> Result<(), String> {
        m.check_format().map_err(|e| e.to_string())?;
        let e = park_in(&root, &mut m, when).map_err(|e| e.to_string())?;
        written.push(e.file);
        Ok(())
    };
    let mut de = serde_json::Deserializer::from_reader(std::io::BufReader::new(file));
    let r = serde::de::DeserializeSeed::deserialize(EachModel(&mut each), &mut de)
        .and_then(|()| de.end());
    match r {
        Ok(()) => {
            let n = written.len();
            match archive_dir().and_then(|d| move_into(&legacy, &d)) {
                Ok(to) => eprintln!(
                    "migrated {n} parked document(s) to {}; the old file is kept as {}",
                    root.display(),
                    to.display()
                ),
                Err(e) => eprintln!("migrated {n} parked document(s); moving the old file: {e}"),
            }
            n
        }
        Err(e) => {
            for w in &written {
                let _ = fs::remove_file(w);
            }
            let aside = legacy.with_file_name(format!("parked_docs.bad.{}.json", now_secs()));
            match fs::rename(&legacy, &aside) {
                Ok(()) => eprintln!(
                    "parked documents unreadable by this build ({e}); kept as {}",
                    aside.display()
                ),
                Err(e2) => eprintln!(
                    "parked documents unreadable ({e}), and moving them aside failed: {e2}"
                ),
            }
            0
        }
    }
}
