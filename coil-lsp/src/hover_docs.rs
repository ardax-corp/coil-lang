//! Hover for builtins, keywords and declarations: a signature block, an
//! import row when the symbol is importable, and its rendered `///` docs.

use super::*;
use crate::docs::{BuiltinDocs, PRELUDE_MODULES, declaration_text, hover_text};

/// Contextual keywords that are also ordinary names (`self.x`, `ensures
/// result > 0`): the type of the name is more useful than keyword docs.
const TYPED_KEYWORDS: &[&str] = &["self", "result"];

/// Hover markdown for the word at `range` when it is a builtin, a keyword or
/// a declaration this server can show the signature of.
pub(crate) fn rich_hover(
    state: &mut ServerState,
    uri: &Uri,
    position: Position,
    text: &str,
    range: &Range<usize>,
) -> Option<String> {
    let name = text.get(range.clone())?;
    let after = &text[range.end..];
    if after.starts_with('!') {
        return macro_hover(state, uri, text, name);
    }
    let ast = Pratt::default().parse(text).ok();
    match qualifier_before(text, range.start) {
        Some((_, ".")) => {
            let (segments, _, access) = member_access_at(text, range.end)?;
            let (class, _) = receiver_class(state, uri, &segments, access)?;
            if let Some(doc) = BuiltinDocs::get().member(&class, name) {
                return Some(doc.markdown());
            }
            let location = member_definition(state, uri, position)?;
            declaration_hover(state, uri, &location, name, false)
        }
        Some((qual, _)) => {
            let docs = BuiltinDocs::get();
            if let Some(doc) = docs.member(&qual, name).or_else(|| docs.item(&qual, name)) {
                return Some(doc.markdown());
            }
            let location = goto_definitions(state, uri, position).into_iter().next()?;
            declaration_hover(state, uri, &location, name, false)
        }
        None => {
            if let Some(markdown) = bare_builtin_hover(ast.as_ref(), text, name) {
                return Some(markdown);
            }
            let location = goto_definitions(state, uri, position)
                .into_iter()
                .next()
                // The declaration itself (methods have no definition lookup).
                .unwrap_or_else(|| Location {
                    uri: uri.clone(),
                    range: byte_range(text, range),
                });
            declaration_hover(state, uri, &location, name, true)
        }
    }
}

/// Without a project: builtins and keywords only.
pub(crate) fn standalone_hover(text: &str, range: &Range<usize>) -> Option<String> {
    let name = text.get(range.clone())?;
    if text[range.end..].starts_with('!') {
        return None;
    }
    let docs = BuiltinDocs::get();
    match qualifier_before(text, range.start) {
        Some((_, ".")) => None,
        Some((qual, _)) => docs
            .member(&qual, name)
            .or_else(|| docs.item(&qual, name))
            .map(|doc| doc.markdown()),
        None => bare_builtin_hover(Pratt::default().parse(text).ok().as_ref(), text, name),
    }
}

/// An unqualified builtin: keyword, primitive type, imported or prelude
/// export, or a prelude enum variant (`Some`, `Err`).
fn bare_builtin_hover(ast: Option<&Output<'_>>, text: &str, name: &str) -> Option<String> {
    let docs = BuiltinDocs::get();
    if !TYPED_KEYWORDS.contains(&name)
        && let Some(body) = docs.keyword(name)
    {
        return Some(hover_text(name, None, Some(body)));
    }
    if let Some(doc) = docs.primitive(name) {
        return Some(doc.markdown());
    }
    let declared_here = ast.is_some_and(|ast| find_decl(ast, text, name, 0).is_some_and(|d| d.top_level));
    if declared_here {
        return None;
    }
    if let Some(ast) = ast {
        if let Some(module) = imported_virtual_module(ast.1.as_ref(), name)
            && let Some(doc) = docs.item(&module, name)
        {
            return Some(doc.markdown());
        }
        if imports_name(ast.1.as_ref(), name) {
            return None;
        }
    }
    PRELUDE_MODULES
        .iter()
        .find_map(|module| docs.item(module, name))
        .or_else(|| {
            ["Option", "Result"]
                .iter()
                .find_map(|owner| docs.member(owner, name))
        })
        .map(|doc| doc.markdown())
}

/// The virtual module a `use` in this file imports `name` from.
fn imported_virtual_module(expression: &Expression<'_>, name: &str) -> Option<String> {
    let modules = VirtualModules::new();
    let mut found = None;
    visit_uses(expression, &mut |path, item, alias| {
        if found.is_some() {
            return;
        }
        let module = path.join("::");
        if item == "*" {
            if modules
                .resolve_glob(path)
                .is_some_and(|exports| exports.iter().any(|e| e.short_name() == name))
            {
                found = Some(module);
            }
        } else if alias.unwrap_or(item) == name && modules.resolve_item(path, item).is_some() {
            found = Some(module);
        }
    });
    found
}

/// Whether some `use` in this file binds `name` (virtual or not).
fn imports_name(expression: &Expression<'_>, name: &str) -> bool {
    let mut found = false;
    visit_uses(expression, &mut |_, item, alias| {
        found |= alias.unwrap_or(item) == name;
    });
    found
}

/// `(path, item, alias)` of one `use`.
type UseVisitor<'a> = dyn FnMut(&[String], &str, Option<&str>) + 'a;

fn visit_uses(expression: &Expression<'_>, f: &mut UseVisitor<'_>) {
    match expression {
        Expression::Program(items) | Expression::Block(items) | Expression::Fragment(items) => {
            for (_, item) in items {
                visit_uses(item, f);
            }
        }
        Expression::Use { path, name, alias } => f(path, name, alias.as_deref()),
        _ => {}
    }
}

/// Signature, import row and docs of the declaration at `location`. The
/// import row is left out for a declaration in the hovered file itself.
fn declaration_hover(
    state: &ServerState,
    from: &Uri,
    location: &Location,
    name: &str,
    bare: bool,
) -> Option<String> {
    let path = uri_path(&location.uri)?;
    let source = source_of(state, &location.uri, &path)?;
    let ast = Pratt::default().parse(&source).ok()?;
    let at = position_to_byte(&source, location.range.start)?;
    let decl = find_decl(&ast, &source, name, at)?;
    // A local function's name hovered bare is the only form `use` can name.
    let import = (bare && decl.top_level && location.uri != *from)
        .then(|| import_line(state, &path, name))
        .flatten();
    let hover = hover_text(&decl.signature, import.as_deref(), decl.docs.as_deref());
    Some(match coverage_note(state, &path, &source, name, at) {
        // Under the signature and import row, above the docs.
        Some(note) => match hover.split_once("\n\n---\n\n") {
            Some((head, docs)) => format!("{head}\n\n{note}\n\n---\n\n{docs}"),
            None => format!("{hover}\n\n{note}"),
        },
        None => hover,
    })
}

fn source_of(state: &ServerState, uri: &Uri, path: &Path) -> Option<String> {
    match state.documents.get(uri) {
        Some(document) => Some(document.text.clone()),
        None => state
            .project_index
            .as_ref()
            .and_then(|index| index.source_for(path).map(str::to_owned))
            .or_else(|| std::fs::read_to_string(path).ok()),
    }
}

pub(crate) struct Decl {
    pub signature: String,
    pub docs: Option<String>,
    /// A module-level item (importable with `use`).
    pub top_level: bool,
}

/// The declaration named `name` whose span holds `at` (`at == 0`: any).
pub(crate) fn find_decl(ast: &Output<'_>, source: &str, name: &str, at: usize) -> Option<Decl> {
    let Expression::Program(items) = ast.1.as_ref() else {
        return None;
    };
    items.iter().find_map(|item| decl_in(item, source, name, at, true))
}

fn decl_in(item: &Output<'_>, source: &str, name: &str, at: usize, top_level: bool) -> Option<Decl> {
    let holds = at == 0 || (item.0.start..item.0.end).contains(&at);
    if !holds {
        return None;
    }
    let decl = |signature: String, docs: &[&str]| Decl {
        signature,
        docs: docs_markdown(docs),
        top_level,
    };
    let header = |item: &Output<'_>| {
        let text = declaration_text(source, item.0.start, item.0.end);
        text.split('{').next().unwrap_or(&text).trim().to_owned()
    };
    match item.1.as_ref() {
        Expression::Function { name: n, docs, body, args, .. } if *n == name => {
            let end = body.as_ref().map_or(item.0.end, |body| body.0.start);
            let mut d = decl(declaration_text(source, item.0.start, end), docs);
            if let Some(parameters) = parameter_docs_markdown(args) {
                d.docs = Some(format!("{}{parameters}", d.docs.unwrap_or_default()));
            }
            Some(d)
        }
        Expression::Function { body: Some(body), .. } if !top_level || at != 0 => {
            // Nested functions are locals: not importable.
            match body.1.as_ref() {
                Expression::Block(items) => items.iter().find_map(|i| decl_in(i, source, name, at, false)),
                _ => None,
            }
        }
        Expression::Method(_, inner) => {
            let mut decl = decl_in(inner, source, name, at, false)?;
            // The outer span keeps `pub` / `static`.
            if let Expression::Function { body: Some(body), .. } = inner.1.as_ref() {
                decl.signature = declaration_text(source, item.0.start, body.0.start);
            }
            Some(decl)
        }
        Expression::Class { name: n, docs, .. }
        | Expression::EnumDecl { name: n, docs, .. }
        | Expression::TypeClass { name: n, docs, .. }
            if *n == name =>
        {
            Some(decl(header(item), docs))
        }
        Expression::TypeAlias { name: n, docs, .. }
        | Expression::FnMacroDecl { name: n, docs, .. }
        | Expression::AttrDecl { name: n, docs, .. }
        | Expression::DeriveDecl { name: n, docs, .. }
            if *n == name =>
        {
            let text = declaration_text(source, item.0.start, item.0.end);
            let signature = match item.1.as_ref() {
                Expression::TypeAlias { .. } => text,
                _ => text.split('{').next().unwrap_or(&text).trim().to_owned(),
            };
            Some(decl(signature, docs))
        }
        Expression::Class { fields, .. } => fields.iter().find_map(|f| field_decl(f, source, name, at)),
        Expression::EnumDecl { name: owner, variants, .. } => variants.iter().find_map(|v| {
            let Expression::EnumVariant { name: n, docs, .. } = v.1.as_ref() else {
                return None;
            };
            (*n == name && (at == 0 || (v.0.start..v.0.end).contains(&at))).then(|| Decl {
                signature: format!(
                    "{owner}::{}",
                    declaration_text(source, v.0.start, v.0.end).trim_end_matches(',')
                ),
                docs: docs_markdown(docs),
                top_level: false,
            })
        }),
        Expression::Implementation { methods, .. } | Expression::TypeClass { methods, .. } => {
            methods.iter().find_map(|m| decl_in(m, source, name, at, false))
        }
        _ => None,
    }
}

fn field_decl(field: &Output<'_>, source: &str, name: &str, at: usize) -> Option<Decl> {
    let Expression::Field { docs, name: field_name, .. } = field.1.as_ref() else {
        return None;
    };
    let Expression::Identifier(n) = field_name.1.as_ref() else {
        return None;
    };
    if *n != name || (at != 0 && !(field.0.start..field.0.end).contains(&at)) {
        return None;
    }
    Some(Decl {
        signature: declaration_text(source, field.0.start, field.0.end)
            .trim_end_matches(',')
            .to_owned(),
        docs: docs_markdown(docs),
        top_level: false,
    })
}

/// `use a::b::name;` for an item of `file`: the module path under the
/// innermost search root, dropping the file name when it is the item's own
/// (`geo/Point.hy` holds `geo::Point`).
fn import_line(state: &ServerState, file: &Path, name: &str) -> Option<String> {
    let root = state.workspace_root.as_ref()?;
    let module = module_roots_of(root, &state.extra_roots)
        .iter()
        .filter_map(|module_root| file.strip_prefix(module_root).ok())
        .map(|rel| {
            rel.with_extension("")
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        })
        .min_by_key(|segments| segments.join("::").len())?;
    let mut segments = module;
    if segments.last().map(String::as_str) != Some(name) {
        segments.push(name.to_owned());
    }
    Some(format!("use {};", segments.join("::")))
}

/// `name!` hover: every `macro name(…)` overload's signature and docs from
/// the module this file imports it from.
fn macro_hover(state: &ServerState, uri: &Uri, text: &str, name: &str) -> Option<String> {
    let ast = Pratt::default().parse(text).ok()?;
    let mut target: Option<(Vec<String>, String)> = None;
    visit_uses(ast.1.as_ref(), &mut |path, item, alias| {
        if target.is_none() && alias.unwrap_or(item) == name && item != "*" {
            target = Some((path.to_vec(), item.to_owned()));
        }
    });
    let (source, file, macro_name) = match target {
        Some((path, item)) => {
            let root = state.workspace_root.as_ref()?;
            let roots = module_roots_of(root, &state.extra_roots);
            let file = compiler::resolve_use_in_roots(&roots, root, &path, &item)?;
            (std::fs::read_to_string(&file).ok()?, Some(file), item)
        }
        // Declared in this file.
        None => (text.to_owned(), uri_path(uri), name.to_owned()),
    };
    let macro_ast = Pratt::default().parse(&source).ok()?;
    let Expression::Program(items) = macro_ast.1.as_ref() else {
        return None;
    };
    let mut signatures = Vec::new();
    let mut docs = Vec::new();
    for item in items {
        if let Expression::FnMacroDecl { name: n, docs: d, body, .. } = item.1.as_ref()
            && *n == macro_name
        {
            signatures.push(declaration_text(&source, item.0.start, body.0.start));
            if let Some(d) = docs_markdown(d) {
                docs.push(d);
            }
        }
    }
    if signatures.is_empty() {
        return None;
    }
    let import = file
        .filter(|file| uri_path(uri).as_ref() != Some(file))
        .and_then(|file| import_line(state, &file, &macro_name));
    Some(hover_text(&signatures.join("\n"), import.as_deref(), Some(&docs.join("\n\n"))))
}

/// Absolute module roots, in search order.
pub(crate) fn module_roots_of(workspace: &Path, extra: &[PathBuf]) -> Vec<PathBuf> {
    lsp_module_roots(workspace, extra)
        .into_iter()
        .map(|r| if r.is_absolute() { r } else { workspace.join(r) })
        .collect()
}
