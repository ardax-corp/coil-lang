//! Source file registry for diagnostic rendering.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Opaque handle to a file registered in a [`SourceMap`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceId(u32);

impl SourceId {
    pub fn as_u32(self) -> u32 {
        self.0
    }
}

/// Maps file paths to source text for pretty and SARIF sinks.
#[derive(Debug, Clone, Default)]
pub struct SourceMap {
    by_path: HashMap<PathBuf, SourceId>,
    entries: Vec<(PathBuf, String)>,
}

impl SourceMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `path` with `text`. Re-inserting the same path updates the
    /// text and returns the existing id.
    pub fn insert(&mut self, path: impl Into<PathBuf>, text: impl Into<String>) -> SourceId {
        let path = path.into();
        let text = text.into();
        if let Some(&id) = self.by_path.get(&path) {
            self.entries[id.0 as usize].1 = text;
            return id;
        }
        let id = SourceId(self.entries.len() as u32);
        self.by_path.insert(path.clone(), id);
        self.entries.push((path, text));
        id
    }

    pub fn get(&self, id: SourceId) -> Option<(&Path, &str)> {
        self.entries
            .get(id.0 as usize)
            .map(|(p, t)| (p.as_path(), t.as_str()))
    }

    pub fn path(&self, id: SourceId) -> Option<&Path> {
        self.get(id).map(|(p, _)| p)
    }

    pub fn text(&self, id: SourceId) -> Option<&str> {
        self.get(id).map(|(_, t)| t)
    }

    /// Iterate all registered sources as `(SourceId, path, text)`.
    pub fn iter(&self) -> impl Iterator<Item = (SourceId, &Path, &str)> {
        self.entries
            .iter()
            .enumerate()
            .map(|(i, (p, t))| (SourceId(i as u32), p.as_path(), t.as_str()))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_returns_stable_id_on_reinsert() {
        let mut map = SourceMap::new();
        let a = map.insert("a.coil", "fn main() {}");
        let b = map.insert("a.coil", "fn main() { }");
        assert_eq!(a, b);
        assert_eq!(map.text(a), Some("fn main() { }"));
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn distinct_paths_get_distinct_ids() {
        let mut map = SourceMap::new();
        let a = map.insert("a.coil", "a");
        let b = map.insert("b.coil", "b");
        assert_ne!(a, b);
        assert_eq!(map.len(), 2);
    }
}
