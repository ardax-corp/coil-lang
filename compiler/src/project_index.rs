//! Project-wide symbol index as a [`DefId`] view over discovered modules.
//!
//! B5: index the use-graph (not every `.hy` under roots). `resolve_definition`
//! uses checker DefId tables, not "every def with this string". Roots are
//! bound on [`Pipeline`] — this module does not `Manifest::load`.

use std::{
    collections::HashMap,
    ops::Range,
    path::{Path, PathBuf},
};

use reporting::Message;

use crate::{
    default_module_roots, manifest::namespace_of_in_roots, Checker, DefId, Pipeline, SymbolIndex,
    SymbolKind,
};

#[derive(Clone)]
struct IndexedFile {
    source: String,
    symbols: SymbolIndex,
}

/// Disk-backed project index wrapping [`Pipeline`] typechecking state.
pub struct ProjectIndex {
    pipeline: Pipeline,
    project_root: PathBuf,
    files: HashMap<PathBuf, IndexedFile>,
}

impl ProjectIndex {
    pub fn new(project_root: PathBuf) -> Self {
        Self::with_roots(project_root, default_module_roots())
    }

    /// Index with explicit search roots (CLI `--root` / LSP workspace).
    pub fn with_roots(project_root: PathBuf, roots: Vec<PathBuf>) -> Self {
        let mut pipeline = Pipeline::new();
        pipeline.bind_project_root(project_root.clone(), roots);
        Self {
            pipeline,
            project_root,
            files: HashMap::new(),
        }
    }

    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    pub fn pipeline(&self) -> &Pipeline {
        &self.pipeline
    }

    pub fn pipeline_mut(&mut self) -> &mut Pipeline {
        &mut self.pipeline
    }

    pub fn checker(&self) -> &Checker {
        self.pipeline.compiler().checker()
    }

    /// Index modules reachable from `entry` (use-graph, not every `.hy`).
    pub fn index_entry(&mut self, entry: &Path) {
        if !entry.exists() {
            return;
        }
        let _ = self.typecheck_entry(entry);
    }

    /// Typecheck the module graph from `entry` and refresh indexed sources
    /// for **discovered** files only.
    pub fn typecheck_entry(&mut self, entry: &Path) -> Vec<(PathBuf, Vec<Message>)> {
        let results = self.pipeline.typecheck_project(entry);
        self.reindex_discovered();
        results
    }

    pub fn upsert_file(&mut self, path: PathBuf, source: String) {
        let mut symbols = SymbolIndex::from_source(path.clone(), &source);
        if let Some(locals) = self.locals_for_file(&path) {
            symbols.bind_def_ids(&locals);
        }
        self.files.insert(path, IndexedFile { source, symbols });
    }

    pub fn source_for(&self, path: &Path) -> Option<&str> {
        self.files.get(path).map(|f| f.source.as_str())
    }

    pub fn symbols_for(&self, path: &Path) -> Option<&SymbolIndex> {
        self.files.get(path).map(|f| &f.symbols)
    }

    /// Resolve a reference site via [`DefId`] / checker tables.
    ///
    /// Untyped buffers (no DefId yet) fall back to **this file's** index
    /// only — never every project def with the same string.
    pub fn resolve_definition(
        &self,
        file: &Path,
        ref_range: Range<usize>,
        name: &str,
    ) -> Vec<(PathBuf, Range<usize>)> {
        let index = match self.files.get(file) {
            Some(f) => &f.symbols,
            None => return Vec::new(),
        };

        let from_site = index
            .references(name)
            .iter()
            .find(|s| s.range == ref_range)
            .and_then(|s| s.def_id)
            .or_else(|| {
                index
                    .all_reference_sites()
                    .find(|s| s.range == ref_range)
                    .and_then(|s| s.def_id)
            });

        if let Some(id) = from_site.or_else(|| index.def_id_for_name(name)) {
            if let Some(loc) = self.location_of(id) {
                return vec![loc];
            }
        }

        index
            .definitions(name)
            .iter()
            .map(|def| (def.file.clone(), def.name_range.clone()))
            .collect()
    }

    fn reindex_discovered(&mut self) {
        let discovered: Vec<PathBuf> = self.pipeline.discovered_files().to_vec();
        self.files
            .retain(|path, _| discovered.iter().any(|d| d == path));
        for path in discovered {
            let source = match std::fs::read_to_string(&path) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let mut symbols = SymbolIndex::from_source(path.clone(), &source);
            if let Some(locals) = self.locals_for_file(&path) {
                symbols.bind_def_ids(&locals);
            }
            self.files.insert(path, IndexedFile { source, symbols });
        }
    }

    fn locals_for_file(&self, path: &Path) -> Option<HashMap<String, DefId>> {
        let ns = if self.pipeline.entry_file() == Some(path) {
            String::new()
        } else {
            namespace_of_in_roots(
                self.pipeline.module_roots(),
                self.pipeline.project_root(),
                path,
            )
            .unwrap_or_default()
        };
        let mid = self.checker().def_interner().module_id(&ns)?;
        self.checker().module_locals(mid).cloned()
    }

    fn location_of(&self, id: DefId) -> Option<(PathBuf, Range<usize>)> {
        let mut alias: Option<(PathBuf, Range<usize>)> = None;
        for file in self.files.values() {
            for def in file.symbols.all_definitions() {
                if def.def_id != Some(id) {
                    continue;
                }
                let loc = (def.file.clone(), def.name_range.clone());
                if def.kind != SymbolKind::Namespace {
                    return Some(loc);
                }
                alias = Some(loc);
            }
        }
        alias
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_project(dir: &Path, files: &[(&str, &str)]) {
        fs::create_dir_all(dir).unwrap();
        for (name, src) in files {
            fs::write(dir.join(name), src).unwrap();
        }
    }

    #[test]
    fn upsert_indexes_function_definition() {
        let dir = std::env::temp_dir().join(format!("coil-project-index-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("main.hy");
        let src = "fn helper() -> int { return 1; }\nfn main() { let x = helper(); }\n";
        fs::write(&path, src).unwrap();

        let mut index = ProjectIndex::new(dir.clone());
        index.upsert_file(path.clone(), src.to_string());
        let symbols = index.symbols_for(&path).expect("indexed");
        let defs = symbols.definitions("helper");
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].kind, crate::SymbolKind::Function);

        let refs = symbols.references("helper");
        assert!(!refs.is_empty(), "expected a reference site for helper()");
        let hit = &refs[0];
        let resolved = index.resolve_definition(&path, hit.range.clone(), "helper");
        assert!(
            resolved
                .iter()
                .any(|(f, r)| f == &path && *r == defs[0].name_range),
            "resolve_definition should return helper's def, got: {resolved:?}"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn upsert_replaces_stale_source() {
        let dir =
            std::env::temp_dir().join(format!("coil-project-index-upd-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("main.hy");

        let mut index = ProjectIndex::new(dir.clone());
        index.upsert_file(path.clone(), "fn old() {}\n".into());
        assert!(index.symbols_for(&path).unwrap().definitions("old").len() == 1);

        index.upsert_file(path.clone(), "fn neu() {}\n".into());
        let symbols = index.symbols_for(&path).unwrap();
        assert!(symbols.definitions("old").is_empty());
        assert_eq!(symbols.definitions("neu").len(), 1);
        assert_eq!(index.source_for(&path), Some("fn neu() {}\n"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn index_entry_uses_use_graph_not_every_hy() {
        let dir = std::env::temp_dir().join(format!(
            "coil-project-index-graph-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        write_project(
            &dir,
            &[
                (
                    "lib.hy",
                    "fn helper() -> int { return 1; }\n",
                ),
                (
                    "main.hy",
                    "use lib::helper;\nfn main() { let x = helper(); return; }\n",
                ),
                (
                    "unused.hy",
                    "fn helper() -> int { return 99; }\nfn never_called() { return; }\n",
                ),
            ],
        );

        let mut index = ProjectIndex::with_roots(dir.clone(), vec![PathBuf::from(".")]);
        index.index_entry(&dir.join("main.hy"));

        let unused = dir.join("unused.hy");
        assert!(
            index.symbols_for(&unused).is_none(),
            "unused.hy is not on the use-graph and must not be indexed"
        );
        let lib = dir.join("lib.hy");
        let main = dir.join("main.hy");
        assert!(index.symbols_for(&lib).is_some(), "lib.hy is imported");
        assert!(index.symbols_for(&main).is_some(), "main.hy is entry");

        let main_syms = index.symbols_for(&main).unwrap();
        let refs: Vec<_> = main_syms
            .references("helper")
            .iter()
            .filter(|s| {
                let src = index.source_for(&main).unwrap();
                &src[s.range.clone()] == "helper"
            })
            .collect();
        assert!(!refs.is_empty(), "expected helper() call site");
        let resolved = index.resolve_definition(&main, refs[0].range.clone(), "helper");
        assert_eq!(
            resolved.len(),
            1,
            "DefId resolve must not return every helper, got {resolved:?}"
        );
        assert_eq!(resolved[0].0, lib);
        assert_eq!(&fs::read_to_string(&lib).unwrap()[resolved[0].1.clone()], "helper");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_modules_defining_foo_do_not_collapse() {
        let dir = std::env::temp_dir().join(format!(
            "coil-project-index-two-foo-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        write_project(
            &dir,
            &[
                ("a.hy", "fn foo() -> int { return 1; }\n"),
                ("b.hy", "fn foo() -> int { return 2; }\n"),
                (
                    "main.hy",
                    "use a::foo;\nuse b::foo as bfoo;\nfn main() { let x = foo(); let y = bfoo(); return; }\n",
                ),
            ],
        );

        let mut index = ProjectIndex::with_roots(dir.clone(), vec![PathBuf::from(".")]);
        index.index_entry(&dir.join("main.hy"));

        let a = dir.join("a.hy");
        let b = dir.join("b.hy");
        let main = dir.join("main.hy");
        let a_defs = index.symbols_for(&a).unwrap().definitions("foo");
        let b_defs = index.symbols_for(&b).unwrap().definitions("foo");
        assert_eq!(a_defs.len(), 1);
        assert_eq!(b_defs.len(), 1);
        let a_id = a_defs[0].def_id;
        let b_id = b_defs[0].def_id;
        assert!(a_id.is_some() && b_id.is_some(), "expected bound DefIds");
        assert_ne!(a_id, b_id, "same-name foo in two modules must not share a DefId");

        let main_syms = index.symbols_for(&main).unwrap();
        let foo_refs: Vec<_> = main_syms
            .references("foo")
            .iter()
            .filter(|s| {
                let src = index.source_for(&main).unwrap();
                &src[s.range.clone()] == "foo"
            })
            .collect();
        assert!(!foo_refs.is_empty());
        let resolved_foo = index.resolve_definition(&main, foo_refs[0].range.clone(), "foo");
        assert_eq!(resolved_foo.len(), 1);
        assert_eq!(resolved_foo[0].0, a);

        let bfoo_refs = main_syms.references("bfoo");
        if !bfoo_refs.is_empty() {
            let resolved_b = index.resolve_definition(&main, bfoo_refs[0].range.clone(), "bfoo");
            assert_eq!(resolved_b.len(), 1);
            assert_eq!(resolved_b[0].0, b);
        }

        let _ = fs::remove_dir_all(&dir);
    }
}
