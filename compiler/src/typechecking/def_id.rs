//! Interned [`ModuleId`] / [`DefId`] for top-level definitions.
//!
//! One intern table covers fns, classes, methods, enums, type aliases,
//! statics, and FFI decls. FFI decls use [`DefKind::Ffi`] as metadata only;
//! the intern key is `(ModuleId, name)` so they share the smaller table
//! rather than a second namespace.

use std::collections::HashMap;

/// Interned module identity. Stable for a compilation session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModuleId(u32);

impl ModuleId {
    pub fn raw(self) -> u32 {
        self.0
    }
}

/// Interned definition identity. One per top-level def in a module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DefId(u32);

impl DefId {
    pub fn raw(self) -> u32 {
        self.0
    }

    pub(crate) fn from_u32(raw: u32) -> Self {
        Self(raw)
    }
}

/// Kind of interned definition (metadata; not part of the intern key).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DefKind {
    Fn,
    Class,
    Method,
    Enum,
    TypeAlias,
    Static,
    /// `extern { fn … }` declaration. Same intern table as other defs.
    Ffi,
}

/// Intern table for modules and top-level defs.
#[derive(Debug, Default)]
pub struct DefInterner {
    modules: HashMap<String, ModuleId>,
    module_names: Vec<String>,
    /// module raw id → name → DefId (overload *set* representative)
    defs: HashMap<u32, HashMap<String, DefId>>,
    /// `(module, name, candidate_id)` → DefId for ABI sidecars (overloads).
    overload_defs: HashMap<(u32, String, u32), DefId>,
    infos: Vec<DefInfo>,
}

/// Recorded intern metadata for a [`DefId`].
#[derive(Debug, Clone)]
pub struct DefInfo {
    pub id: DefId,
    pub module: ModuleId,
    pub kind: DefKind,
    pub name: String,
}

impl DefInterner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Intern `path` (empty string is the entry module). Stable.
    pub fn intern_module(&mut self, path: &str) -> ModuleId {
        if let Some(&id) = self.modules.get(path) {
            return id;
        }
        let id = ModuleId(self.module_names.len() as u32);
        self.modules.insert(path.to_string(), id);
        self.module_names.push(path.to_string());
        id
    }

    pub fn module_id(&self, path: &str) -> Option<ModuleId> {
        self.modules.get(path).copied()
    }

    pub fn module_path(&self, id: ModuleId) -> Option<&str> {
        self.module_names.get(id.0 as usize).map(String::as_str)
    }

    /// Intern a top-level def. Re-interning the same `(module, name)`
    /// returns the original [`DefId`]; `kind` is kept from the first insert.
    /// This id is the overload *set* representative (`local_defs` short name).
    pub fn intern(&mut self, module: ModuleId, kind: DefKind, name: &str) -> DefId {
        if let Some(&id) = self.defs.get(&module.0).and_then(|m| m.get(name)) {
            return id;
        }
        let id = DefId(self.infos.len() as u32);
        self.defs
            .entry(module.0)
            .or_default()
            .insert(name.to_string(), id);
        self.infos.push(DefInfo {
            id,
            module,
            kind,
            name: name.to_string(),
        });
        self.overload_defs
            .entry((module.0, name.to_string(), 0))
            .or_insert(id);
        id
    }

    /// Distinct [`DefId`] for one overload candidate. Candidate `0` is the
    /// set representative from [`intern`].
    pub fn intern_overload(
        &mut self,
        module: ModuleId,
        kind: DefKind,
        name: &str,
        candidate: u32,
    ) -> DefId {
        if let Some(&id) = self
            .overload_defs
            .get(&(module.0, name.to_string(), candidate))
        {
            return id;
        }
        if candidate == 0 {
            return self.intern(module, kind, name);
        }
        let id = DefId(self.infos.len() as u32);
        self.overload_defs
            .insert((module.0, name.to_string(), candidate), id);
        self.infos.push(DefInfo {
            id,
            module,
            kind,
            name: name.to_string(),
        });
        id
    }

    pub fn get(&self, module: ModuleId, name: &str) -> Option<DefId> {
        self.defs.get(&module.0)?.get(name).copied()
    }

    pub fn get_overload(&self, module: ModuleId, name: &str, candidate: u32) -> Option<DefId> {
        self.overload_defs
            .get(&(module.0, name.to_string(), candidate))
            .copied()
            .or_else(|| {
                if candidate == 0 {
                    self.get(module, name)
                } else {
                    None
                }
            })
    }

    pub fn info(&self, id: DefId) -> Option<&DefInfo> {
        self.infos.get(id.0 as usize)
    }

    pub fn len(&self) -> usize {
        self.infos.len()
    }

    pub fn is_empty(&self) -> bool {
        self.infos.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &DefInfo> {
        self.infos.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_two_fns_def_ids_differ() {
        let mut intern = DefInterner::new();
        let m = intern.intern_module("a");
        let foo = intern.intern(m, DefKind::Fn, "foo");
        let bar = intern.intern(m, DefKind::Fn, "bar");
        assert_ne!(foo, bar);
        assert_eq!(intern.info(foo).unwrap().name, "foo");
        assert_eq!(intern.info(bar).unwrap().name, "bar");
    }

    #[test]
    fn intern_overloads_get_distinct_def_ids() {
        let mut intern = DefInterner::new();
        let m = intern.intern_module("a");
        let set = intern.intern(m, DefKind::Fn, "f");
        let c0 = intern.intern_overload(m, DefKind::Fn, "f", 0);
        let c1 = intern.intern_overload(m, DefKind::Fn, "f", 1);
        assert_eq!(c0, set);
        assert_ne!(c1, set);
        assert_eq!(intern.get(m, "f"), Some(set));
        assert_eq!(intern.get_overload(m, "f", 1), Some(c1));
    }

    #[test]
    fn intern_same_logical_def_twice_same_id() {
        let mut intern = DefInterner::new();
        let m = intern.intern_module("a");
        let first = intern.intern(m, DefKind::Fn, "foo");
        let second = intern.intern(m, DefKind::Fn, "foo");
        assert_eq!(first, second);
        assert_eq!(intern.len(), 1);
    }

    #[test]
    fn intern_module_is_stable() {
        let mut intern = DefInterner::new();
        let a = intern.intern_module("math");
        let b = intern.intern_module("math");
        let c = intern.intern_module("text");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
