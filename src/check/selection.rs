//! Select which independent checks contribute work and findings.

use clap::ValueEnum;

/// Public names accepted by `check --checks`.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Kind {
    Folders,
    Age,
    Content,
}

/// Enabled checks; all are enabled when the option is omitted.
#[derive(Clone, Copy, Debug)]
pub struct Selection {
    pub folders: bool,
    pub age: bool,
    pub content: bool,
}

impl Default for Selection {
    /// Preserve the behavior of check and report without an explicit selection.
    fn default() -> Self {
        Self {
            folders: true,
            age: true,
            content: true,
        }
    }
}

impl Selection {
    /// Convert CLI values into flags, treating duplicate names as one check.
    pub fn from_kinds(kinds: &[Kind]) -> Self {
        Self {
            folders: kinds.iter().any(|kind| matches!(kind, Kind::Folders)),
            age: kinds.iter().any(|kind| matches!(kind, Kind::Age)),
            content: kinds.iter().any(|kind| matches!(kind, Kind::Content)),
        }
    }
}
