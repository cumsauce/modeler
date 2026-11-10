//! The crate's error type. Messages are the user-facing text (the MCP layer
//! sends `to_string()`), while the variants let callers and tests match on the
//! cause instead of substrings.

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Error)]
pub enum ModelerError {
    /// A part name that does not exist in the model.
    #[error("no part `{0}`")]
    NoPart(String),
    /// An op input that no earlier op in the graph produces.
    #[error("no part `{0}` earlier in the graph")]
    MissingInput(String),
    /// The op graph has a dependency cycle (names of the stuck ops).
    #[error("ops form a dependency cycle involving {0}")]
    Cycle(String),
    /// A boolean/CSG failure (empty result, csgrs panic, ...).
    #[error("{0}")]
    Csg(String),
    /// Filesystem trouble (catalog, autosave, export).
    #[error("{0}")]
    Io(String),
    /// Malformed saved data.
    #[error("{0}")]
    Parse(String),
    /// Bad parameters or an operation that does not apply to the current state.
    #[error("{0}")]
    Invalid(String),
}

impl From<String> for ModelerError {
    fn from(s: String) -> Self {
        ModelerError::Invalid(s)
    }
}

impl From<&str> for ModelerError {
    fn from(s: &str) -> Self {
        ModelerError::Invalid(s.to_string())
    }
}

pub type Result<T> = std::result::Result<T, ModelerError>;
