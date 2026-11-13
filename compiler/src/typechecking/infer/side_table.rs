//! [`SideTable`]: a checker fact per expression, by [`NodeId`] and by span.

use std::collections::HashMap;

use crate::typechecking::id::NodeId;

/// A fact the checker records per expression (a call's dictionaries, a
/// `for` loop's lowering), by the expression's [`NodeId`] and by its source
/// span `(start, end)`. Codegen asks by id, and by span where the pre-walk
/// and inference ids disagree (function bodies).
#[derive(Clone, Debug)]
pub(crate) struct SideTable<T> {
    by_id: HashMap<NodeId, T>,
    by_span: HashMap<(usize, usize), T>,
}

impl<T> Default for SideTable<T> {
    fn default() -> Self {
        Self { by_id: HashMap::new(), by_span: HashMap::new() }
    }
}

impl<T: Clone> SideTable<T> {
    /// Record `value` at `span`, and at `id` when the expression has one.
    pub(crate) fn insert(&mut self, id: Option<NodeId>, span: (usize, usize), value: T) {
        if let Some(id) = id {
            self.by_id.insert(id, value.clone());
        }
        self.by_span.insert(span, value);
    }

    pub(crate) fn at(&self, id: NodeId) -> Option<&T> {
        self.by_id.get(&id)
    }

    pub(crate) fn for_span(&self, start: usize, end: usize) -> Option<&T> {
        self.by_span.get(&(start, end))
    }

    /// The facts by id (the typed sidecar's snapshot).
    pub(crate) fn by_id(&self) -> &HashMap<NodeId, T> {
        &self.by_id
    }

    pub(crate) fn clear(&mut self) {
        self.by_id.clear();
        self.by_span.clear();
    }
}

impl<T: Clone> SideTable<Vec<T>> {
    /// Append `item` to the list at `span`, and at `id` when there is one.
    pub(crate) fn push(&mut self, id: Option<NodeId>, span: (usize, usize), item: T) {
        if let Some(id) = id {
            self.by_id.entry(id).or_default().push(item.clone());
        }
        self.by_span.entry(span).or_default().push(item);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fact_is_found_by_id_and_by_span() {
        let mut t = SideTable::default();
        t.insert(Some(NodeId(3)), (1, 4), "a");
        t.insert(None, (5, 6), "b");
        assert_eq!(t.at(NodeId(3)), Some(&"a"));
        assert_eq!(t.for_span(1, 4), Some(&"a"));
        assert_eq!(t.for_span(5, 6), Some(&"b"));
        assert_eq!(t.by_id().len(), 1);
        let mut v = SideTable::default();
        v.push(Some(NodeId(1)), (0, 1), 7);
        v.push(Some(NodeId(1)), (0, 1), 8);
        assert_eq!(v.at(NodeId(1)), Some(&vec![7, 8]));
        t.clear();
        assert!(t.for_span(1, 4).is_none());
    }
}
