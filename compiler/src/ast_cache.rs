//! Per-file parsed (then expanded) AST cache.
//!
//! Discover and compile used to parse every file twice. The pipeline now keeps
//! the expanded AST for the rest of the session so `parse → expand → check`
//! is one function for both compile and `typecheck_project`.

use std::pin::Pin;
use std::path::{Path, PathBuf};

use parser::{Pratt, ast::Output};
use reporting::Message;

use crate::attrs::{ExpandResult, expand_program};

/// Source plus expanded AST for one file. `ast` borrows pinned `source` (and
/// `'static` strings leaked by attr expand). Field order drops `ast` first.
pub struct CachedAst {
    ast: Option<Output<'static>>,
    expand: ExpandResult,
    parse_error: Option<Message>,
    expanded: bool,
    checked: bool,
    source: Pin<Box<str>>,
}

impl CachedAst {
    pub fn parse(source: String) -> Self {
        let source = Pin::from(source.into_boxed_str());
        match Pratt::default().parse(&*source) {
            Ok(ast) => {
                // SAFETY: `ast` borrows the pinned `Box<str>`. The pin is never
                // moved out of this struct; `ast` is dropped first.
                let ast = unsafe { extend_ast_lifetime(ast) };
                Self {
                    ast: Some(ast),
                    expand: ExpandResult::default(),
                    parse_error: None,
                    expanded: false,
                    checked: false,
                    source,
                }
            }
            Err(parse_error) => Self {
                ast: None,
                expand: ExpandResult::default(),
                parse_error: Some(parse_error),
                expanded: false,
                checked: false,
                source,
            },
        }
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn parse_error(&self) -> Option<&Message> {
        self.parse_error.as_ref()
    }

    pub fn expanded(&self) -> bool {
        self.expanded
    }

    pub fn checked(&self) -> bool {
        self.checked
    }

    pub fn mark_checked(&mut self) {
        self.checked = true;
        self.expanded = true;
    }

    pub fn ast(&self) -> Option<&Output<'static>> {
        self.ast.as_ref()
    }

    pub fn ast_mut(&mut self) -> Option<&mut Output<'static>> {
        self.ast.as_mut()
    }

    /// Expand attributes in place. Idempotent.
    pub fn expand_if_needed(&mut self) -> &ExpandResult {
        if !self.expanded
            && let Some(ast) = self.ast.as_mut()
        {
            self.expand = expand_program(ast);
            self.expanded = true;
        }
        &self.expand
    }

    pub fn take_expand(&mut self) -> ExpandResult {
        self.expand_if_needed();
        std::mem::take(&mut self.expand)
    }
}

/// Session cache keyed by normalized path.
#[derive(Default)]
pub struct AstCache {
    files: std::collections::HashMap<PathBuf, CachedAst>,
}

impl AstCache {
    pub fn clear(&mut self) {
        self.files.clear();
    }

    pub fn remove(&mut self, file: &Path) {
        self.files.remove(file);
    }

    pub fn get(&self, file: &Path) -> Option<&CachedAst> {
        self.files.get(file)
    }

    pub fn get_mut(&mut self, file: &Path) -> Option<&mut CachedAst> {
        self.files.get_mut(file)
    }

    pub fn insert(&mut self, file: PathBuf, cached: CachedAst) {
        self.files.insert(file, cached);
    }
}

/// SAFETY: caller pins the source `Box<str>` for longer than `ast`.
unsafe fn extend_ast_lifetime(ast: Output<'_>) -> Output<'static> {
    unsafe { std::mem::transmute(ast) }
}
