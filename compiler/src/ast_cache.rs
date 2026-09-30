//! Per-file parsed (then expanded) AST cache.
//!
//! Discover and compile used to parse every file twice. The pipeline now keeps
//! the expanded AST for the rest of the session so `parse → expand → check`
//! is one function for both compile and `typecheck_project`.

use std::pin::Pin;
use std::path::{Path, PathBuf};

use parser::{Pratt, ast::Output};
use reporting::Message;

use crate::attrs::{ExpandResult, expand_program_in};
use crate::macros::{MacroDecl, PendingMacro};

/// Source plus expanded AST for one file. `ast` borrows pinned `source` (and
/// `'static` strings leaked by attr expand). Field order drops `ast` first.
pub struct CachedAst {
    ast: Option<Output<'static>>,
    /// Macro output parsed into `ast`: each is padded so its spans sit past
    /// the end of `source` (and of earlier snippets). Dropped after `ast`.
    generated: Vec<Pin<Box<str>>>,
    /// `"\n" + snippet` for each generated snippet, in order.
    generated_text: String,
    macro_decls: Vec<MacroDecl>,
    expand: ExpandResult,
    parse_error: Option<Message>,
    expanded: bool,
    checked: bool,
    source: Pin<Box<str>>,
}

impl CachedAst {
    pub fn parse(source: String) -> Self {
        let source = Pin::from(source.into_boxed_str());
        match Pratt::default().parse(&source) {
            Ok(ast) => {
                // SAFETY: `ast` borrows the pinned `Box<str>`. The pin is never
                // moved out of this struct; `ast` is dropped first.
                let ast = unsafe { extend_ast_lifetime(ast) };
                Self {
                    ast: Some(ast),
                    generated: Vec::new(),
                    generated_text: String::new(),
                    macro_decls: Vec::new(),
                    expand: ExpandResult::default(),
                    parse_error: None,
                    expanded: false,
                    checked: false,
                    source,
                }
            }
            Err(parse_error) => Self {
                ast: None,
                generated: Vec::new(),
                generated_text: String::new(),
                macro_decls: Vec::new(),
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

    /// Expand attributes in place (module path `""`). Idempotent.
    pub fn expand_if_needed(&mut self) -> &ExpandResult {
        self.expand_in("")
    }

    /// Expand attributes in place for a file of module `module`. Idempotent.
    pub fn expand_in(&mut self, module: &str) -> &ExpandResult {
        if !self.expanded
            && let Some(ast) = self.ast.as_mut()
        {
            self.expand = expand_program_in(ast, module);
            self.macro_decls = self.expand.macro_decls.clone();
            self.expanded = true;
        }
        &self.expand
    }

    pub fn take_expand(&mut self) -> ExpandResult {
        self.expand_if_needed();
        std::mem::take(&mut self.expand)
    }

    /// `derive` / macro `attr` items this file declares.
    pub fn macro_decls(&self) -> &[MacroDecl] {
        &self.macro_decls
    }

    /// User macro uses still waiting for the pipeline.
    pub fn pending_macros(&self) -> &[PendingMacro] {
        &self.expand.pending
    }

    /// Take the pending user macro uses (the pipeline resolves or reports them).
    pub fn take_pending_macros(&mut self) -> Vec<PendingMacro> {
        std::mem::take(&mut self.expand.pending)
    }

    /// Add diagnostics produced while resolving this file's macros.
    pub fn push_expand_messages(&mut self, messages: impl IntoIterator<Item = Message>) {
        self.expand.messages.extend(messages);
    }

    /// Source plus every generated snippet: what diagnostics are rendered against.
    pub fn report_source(&self) -> String {
        if self.generated_text.is_empty() {
            return self.source.to_string();
        }
        format!("{}{}", &*self.source, self.generated_text)
    }

    /// Parse `snippet` (macro output) with spans placed after all earlier
    /// text, so they never collide with the file's own spans and diagnostics
    /// can be rendered against [`Self::report_source`].
    pub fn parse_generated(
        &mut self,
        snippet: &str,
    ) -> Result<(Output<'static>, std::ops::Range<usize>), Message> {
        let offset = self.source.len() + self.generated_text.len() + 1;
        let padded: Pin<Box<str>> =
            Pin::from(format!("{}{snippet}", " ".repeat(offset)).into_boxed_str());
        self.generated_text.push('\n');
        self.generated_text.push_str(snippet);
        let range = offset..offset + snippet.len();
        let parsed = Pratt::default().parse(&padded);
        // SAFETY: the padded text is pinned in `generated` for as long as the
        // cache entry lives, and `ast` (which takes these nodes) drops first.
        let parsed = parsed.map(|ast| unsafe { extend_ast_lifetime(ast) });
        self.generated.push(padded);
        parsed.map(|ast| (ast, range))
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

    pub fn values(&self) -> impl Iterator<Item = &CachedAst> {
        self.files.values()
    }
}

/// SAFETY: caller pins the source `Box<str>` for longer than `ast`.
unsafe fn extend_ast_lifetime(ast: Output<'_>) -> Output<'static> {
    unsafe { std::mem::transmute(ast) }
}
