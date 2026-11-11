//! Friction log: what went wrong for agents driving the server, kept on
//! this machine in `<catalog dir>/feedback.jsonl` (one JSON object per line,
//! never sent anywhere).
//!
//! Two sources:
//! - **automatic**: every failed tool call (with its error and a clip of its
//!   arguments, and how many times in a row that tool has failed) and every
//!   call slower than [`SLOW_MS`];
//! - **notes**: free text an agent sends with `session_log(feedback: ...)`
//!   when something surprised it, with the calls just before it attached.
//!
//! [`report`] folds it into what to fix first (`session_log(report: true)`,
//! or `modeler --feedback-report` from a shell).

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Calls slower than this are logged (renders wait up to 15 s).
pub const SLOW_MS: u128 = 8000;

/// How much of a call's arguments and error text an entry keeps.
const CLIP: usize = 600;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    /// Seconds since the Unix epoch.
    pub ts: u64,
    /// "error", "slow" or "note".
    pub kind: String,
    /// The tool called (for notes: the last tool before the note).
    #[serde(default)]
    pub tool: String,
    /// The error text, the duration, or the note itself.
    pub detail: String,
    /// A clip of the call's arguments (errors and slow calls).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub args: String,
    /// Failures of this tool in a row, this one included (errors).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub streak: u32,
    /// The calls just before (notes): "tool ok|ERROR (ms)".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent: Vec<String>,
    /// The server build (package version + binary modification time).
    #[serde(default)]
    pub build: String,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

pub fn path() -> Option<PathBuf> {
    crate::catalog::catalog_dir()
        .ok()
        .map(|d| d.join("feedback.jsonl"))
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn build() -> String {
    let mtime = std::env::current_exe()
        .and_then(std::fs::metadata)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{}@{mtime}", env!("CARGO_PKG_VERSION"))
}

pub fn clip(s: &str) -> String {
    if s.chars().count() <= CLIP {
        return s.to_string();
    }
    s.chars().take(CLIP).collect::<String>() + "…"
}

/// Appends one entry (timestamp and build filled in). Errors are ignored:
/// the log must never break a tool call.
pub fn record(mut e: Entry) {
    e.ts = now();
    e.build = build();
    e.detail = clip(&e.detail);
    e.args = clip(&e.args);
    let Some(p) = path() else {
        return;
    };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Ok(line) = serde_json::to_string(&e) else {
        return;
    };
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&p) {
        let _ = writeln!(f, "{line}");
    }
}

pub fn read() -> Vec<Entry> {
    let Some(text) = path().and_then(|p| std::fs::read_to_string(p).ok()) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// An error's first line with numbers and quoted names blanked, so the same
/// problem on different parts groups together.
fn signature(detail: &str) -> String {
    let first = detail.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let mut out = String::new();
    let mut quoted = None;
    for c in first.chars() {
        match quoted {
            Some(q) if c == q => {
                out.push(c);
                quoted = None;
            }
            Some(_) => {}
            None if c == '`' || c == '\'' || c == '"' => {
                out.push(c);
                out.push('…');
                quoted = Some(c);
            }
            None if c.is_ascii_digit() => {
                if !out.ends_with('#') {
                    out.push('#');
                }
            }
            None => out.push(c),
        }
    }
    out.chars().take(140).collect()
}

/// What to fix first: errors grouped by tool and message (most frequent
/// first, with the longest failure streak and one example's arguments),
/// slow calls by tool, then the agents' notes, newest first. `since`
/// limits it to entries newer than that many days.
pub fn report(entries: &[Entry], since_days: Option<u64>) -> String {
    let cutoff = since_days.map(|d| now().saturating_sub(d * 86_400));
    let es: Vec<&Entry> = entries
        .iter()
        .filter(|e| cutoff.is_none_or(|c| e.ts >= c))
        .collect();
    if es.is_empty() {
        return "no feedback recorded yet".into();
    }
    let mut out = String::new();
    // (tool, signature) -> (count, max streak, last example)
    let mut errors: BTreeMap<(String, String), (usize, u32, &Entry)> = BTreeMap::new();
    let mut slow: BTreeMap<String, (usize, u128)> = BTreeMap::new();
    let mut notes: Vec<&Entry> = Vec::new();
    for e in &es {
        match e.kind.as_str() {
            "error" => {
                let g = errors
                    .entry((e.tool.clone(), signature(&e.detail)))
                    .or_insert((0, 0, e));
                g.0 += 1;
                g.1 = g.1.max(e.streak);
                g.2 = e;
            }
            "slow" => {
                let ms = e
                    .detail
                    .split_whitespace()
                    .next()
                    .and_then(|v| v.trim_end_matches("ms").parse::<u128>().ok())
                    .unwrap_or(0);
                let g = slow.entry(e.tool.clone()).or_insert((0, 0));
                g.0 += 1;
                g.1 = g.1.max(ms);
            }
            _ => notes.push(e),
        }
    }
    let n_err: usize = errors.values().map(|g| g.0).sum();
    out += &format!(
        "{} entries: {n_err} failed call(s) in {} kind(s), {} slow call(s), {} note(s)\n",
        es.len(),
        errors.len(),
        slow.values().map(|g| g.0).sum::<usize>(),
        notes.len()
    );
    if !errors.is_empty() {
        out += "\nfailures (most frequent first):\n";
        let mut v: Vec<_> = errors.into_iter().collect();
        v.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(b.1.1.cmp(&a.1.1)));
        for ((tool, sig), (n, streak, ex)) in v {
            out += &format!("- {n}x {tool}: {sig}");
            if streak > 1 {
                out += &format!(" (up to {streak} failures in a row)");
            }
            out += "\n";
            if !ex.args.is_empty() {
                out += &format!("    e.g. args {}\n", clip_line(&ex.args, 200));
            }
        }
    }
    if !slow.is_empty() {
        out += "\nslow calls:\n";
        for (tool, (n, max)) in slow {
            out += &format!("- {n}x {tool} (slowest {:.1} s)\n", max as f64 / 1000.0);
        }
    }
    if !notes.is_empty() {
        out += "\nagent notes (newest first):\n";
        for e in notes.iter().rev() {
            out += &format!("- {}\n", e.detail.replace('\n', "\n  "));
            if !e.recent.is_empty() {
                out += &format!("    before it: {}\n", e.recent.join(", "));
            }
        }
    }
    out
}

fn clip_line(s: &str, n: usize) -> String {
    let one = s.replace('\n', " ");
    if one.chars().count() <= n {
        one
    } else {
        one.chars().take(n).collect::<String>() + "…"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(kind: &str, tool: &str, detail: &str, streak: u32) -> Entry {
        Entry {
            ts: now(),
            kind: kind.into(),
            tool: tool.into(),
            detail: detail.into(),
            streak,
            ..Default::default()
        }
    }

    #[test]
    fn the_report_groups_the_same_failure_on_different_parts() {
        let es = vec![
            e(
                "error",
                "apply_ops",
                "batch item #23 failed: no part `wall_*`",
                1,
            ),
            e(
                "error",
                "apply_ops",
                "batch item #4 failed: no part `pil_*`",
                2,
            ),
            e(
                "error",
                "material_ops",
                "node `tint` (random): out: `value`",
                1,
            ),
            e("slow", "agent_views", "9100 ms", 0),
            e("note", "apply_ops", "material items don't take globs", 0),
        ];
        let r = report(&es, None);
        assert!(
            r.contains("2x apply_ops: batch item # failed: no part `…`"),
            "{r}"
        );
        assert!(r.contains("up to 2 failures in a row"), "{r}");
        assert!(r.contains("1x agent_views (slowest 9.1 s)"), "{r}");
        assert!(r.contains("material items don't take globs"), "{r}");
    }
}
