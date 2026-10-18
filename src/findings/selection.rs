//! Select which independent checks contribute work and findings.

use clap::ValueEnum;
use serde::Deserialize;

/// Public names accepted by CLI selection and repository silencing.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Folders,
    Age,
    Content,
}

impl Kind {
    /// Stable label shared by bracketed tree details and report lines.
    pub fn name(self) -> &'static str {
        match self {
            Self::Folders => "folders",
            Self::Age => "age",
            Self::Content => "content",
        }
    }
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
    /// Remove repository-silenced checks without enabling any unselected checks.
    pub fn without(mut self, silenced: &[Kind]) -> Self {
        for kind in silenced {
            match kind {
                Kind::Folders => self.folders = false,
                Kind::Age => self.age = false,
                Kind::Content => self.content = false,
            }
        }
        self
    }

    /// Convert CLI values into flags, treating duplicate names as one check.
    pub fn from_kinds(kinds: &[Kind]) -> Self {
        Self {
            folders: kinds.iter().any(|kind| matches!(kind, Kind::Folders)),
            age: kinds.iter().any(|kind| matches!(kind, Kind::Age)),
            content: kinds.iter().any(|kind| matches!(kind, Kind::Content)),
        }
    }
}
