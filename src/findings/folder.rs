//! Structural inventory nodes, without check evaluation or presentation logic.

use std::collections::BTreeMap;

use super::Repository;

/// Structural tree node shared by repository scanning, evaluation, and tree rendering.
#[derive(Debug, Default)]
pub struct Folder {
    pub(crate) repository: Option<Repository>,
    pub(crate) children: BTreeMap<String, Folder>,
    pub(crate) file_count: usize,
}

impl Folder {
    /// Create missing ancestors, ignoring empty path components, and return the node.
    pub(super) fn at(&mut self, path: &str) -> &mut Self {
        let mut folder = self;
        for component in path.split('/').filter(|part| !part.is_empty()) {
            folder = folder.children.entry(component.to_owned()).or_default();
        }
        folder
    }

    /// Count one object in its displayed folder and every ancestor, excluding markers.
    pub(super) fn count_file(&mut self, path: &str) {
        self.file_count += 1;
        let mut folder = self;
        for component in path.split('/').filter(|part| !part.is_empty()) {
            folder = folder.children.entry(component.to_owned()).or_default();
            folder.file_count += 1;
        }
    }

    /// Whether this node or any descendant is a configured repository.
    pub(super) fn has_repository(&self) -> bool {
        self.repository.is_some() || self.children.values().any(Self::has_repository)
    }
}
