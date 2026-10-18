//! Collect repository measurements, evaluate shared findings, and render their folder tree.

/// Repository-specific byte-pattern validation.
pub mod content;
/// Structured check outcomes and shared evaluation.
pub mod findings;
/// Tree presentation of already-evaluated findings.
mod render;
/// Repository measurements and check conditions.
mod repository;
/// Recursive storage inspection.
mod scan;
/// CLI check names and execution flags.
pub mod selection;

pub use findings::{Finding, findings};
pub use render::render;
pub use repository::Repository;
pub use scan::inspect;

use std::collections::BTreeMap;

/// Structural tree node; check evaluation belongs to Repository, not Folder.
#[derive(Debug, Default)]
pub struct Folder {
    repository: Option<Repository>,
    children: BTreeMap<String, Folder>,
    file_count: usize,
}

impl Folder {
    /// Create missing ancestors, ignoring empty path components, and return the node.
    fn at(&mut self, path: &str) -> &mut Self {
        let mut folder = self;
        for component in path.split('/').filter(|part| !part.is_empty()) {
            folder = folder.children.entry(component.to_owned()).or_default();
        }
        folder
    }

    /// Count one object in its displayed folder and every ancestor, excluding markers.
    fn count_file(&mut self, path: &str) {
        self.file_count += 1;
        let mut folder = self;
        for component in path.split('/').filter(|part| !part.is_empty()) {
            folder = folder.children.entry(component.to_owned()).or_default();
            folder.file_count += 1;
        }
    }

    /// Whether this node or any descendant is a configured repository.
    fn has_repository(&self) -> bool {
        self.repository.is_some() || self.children.values().any(Self::has_repository)
    }
}

/// Scan behavior, shared evaluation, and presentation grouped by responsibility.
#[cfg(test)]
mod tests;
