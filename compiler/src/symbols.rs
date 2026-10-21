use std::{
    collections::HashMap,
    ops::Range,
    path::{Path, PathBuf},
};

use parser::ast::{EnumConstructPayload, EnumVariantPayload, Expression, Output};

use crate::DefId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Function,
    Variable,
    Class,
    Enum,
    TypeAlias,
    Namespace,
    Method,
    Macro,
    Trait,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolDef {
    pub name: String,
    pub kind: SymbolKind,
    pub file: PathBuf,
    pub range: Range<usize>,
    pub name_range: Range<usize>,
    /// Interned def when this index was bound to checker tables (B5).
    pub def_id: Option<DefId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefSite {
    pub name: String,
    pub file: PathBuf,
    pub range: Range<usize>,
    /// Bound [`DefId`] for this use, when resolve tables are present.
    pub def_id: Option<DefId>,
    /// The segment of an `Owner::member` path this site is, if any.
    pub path: Option<PathPart>,
}

/// A segment of an `Owner::member` path (`Area::size(x)`, `Color::Red`).
///
/// The owner site is named by the owner's last segment (`Area`); the
/// member site by `Owner::member`, which is also the key the owner's
/// members are defined under (trait methods, inherent `impl` methods).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathPart {
    Owner,
    Member,
}

#[derive(Debug, Clone, Default)]
pub struct SymbolIndex {
    definitions: HashMap<String, Vec<SymbolDef>>,
    references: HashMap<String, Vec<RefSite>>,
}

impl SymbolIndex {
    pub fn from_source(file: PathBuf, source: &str) -> Self {
        let Ok(root) = parser::Pratt::default().parse(source) else {
            return Self::default();
        };
        let mut index = Self::default();
        index.collect_definitions(&file, source, &root);
        index.collect_references(&file, source, &root);
        index
    }

    pub fn definitions(&self, name: &str) -> &[SymbolDef] {
        self.definitions.get(name).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn references(&self, name: &str) -> &[RefSite] {
        self.references.get(name).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn all_definitions(&self) -> impl Iterator<Item = &SymbolDef> {
        self.definitions.values().flatten()
    }

    pub fn all_reference_sites(&self) -> impl Iterator<Item = &RefSite> {
        self.references.values().flatten()
    }

    fn collect_definitions(&mut self, file: &Path, source: &str, expression: &Output<'_>) {
        let Expression::Program(items) = expression.1.as_ref() else {
            return;
        };
        for item in items {
            let (span, expression) = item;
            let (name, kind) = match expression.as_ref() {
                Expression::Function { name, .. } => (*name, SymbolKind::Function),
                Expression::Class { name, .. } => (*name, SymbolKind::Class),
                Expression::EnumDecl { name, .. } => (*name, SymbolKind::Enum),
                Expression::TypeAlias { name, .. } => (*name, SymbolKind::TypeAlias),
                Expression::StaticDecl { name, .. } => (*name, SymbolKind::Variable),
                Expression::AttrDecl { name, .. } => (*name, SymbolKind::Method),
                Expression::DeriveDecl { name, .. } | Expression::FnMacroDecl { name, .. } => {
                    (*name, SymbolKind::Macro)
                }
                Expression::Use { name, alias, .. } => {
                    (alias.as_deref().unwrap_or(name), SymbolKind::Namespace)
                }
                Expression::TypeClass { name, methods, .. } => {
                    self.collect_members(file, source, name, methods);
                    (*name, SymbolKind::Trait)
                }
                Expression::Implementation { owner, methods, .. } => {
                    self.collect_members(file, source, owner, methods);
                    continue;
                }
                _ => continue,
            };
            let range = span.start..span.end;
            // Prefer the parser's source slice; fall back to a search *inside
            // this item span* (not the whole file). `use` aliases take the
            // rightmost hit so `out` is not the `out` in `stdout`.
            let prefer_last = matches!(expression.as_ref(), Expression::Use { .. });
            let name_range = name_range_in(source, name, &range, prefer_last);
            let definition = SymbolDef {
                name: name.to_owned(),
                kind,
                file: file.to_path_buf(),
                range,
                name_range,
                def_id: None,
            };
            self.definitions
                .entry(name.to_owned())
                .or_default()
                .push(definition);
        }
    }

    /// Methods of a trait or inherent `impl`, keyed `Owner::method`.
    fn collect_members(&mut self, file: &Path, source: &str, owner: &str, methods: &[Output<'_>]) {
        for method in methods {
            let range = method.0.start..method.0.end;
            let inner = match method.1.as_ref() {
                Expression::Method(_, inner) => inner.1.as_ref(),
                other => other,
            };
            let Expression::Function { name, .. } = inner else {
                continue;
            };
            let key = format!("{owner}::{name}");
            let definition = SymbolDef {
                name: key.clone(),
                kind: SymbolKind::Method,
                file: file.to_path_buf(),
                name_range: name_range_in(source, name, &range, false),
                range,
                def_id: None,
            };
            self.definitions.entry(key).or_default().push(definition);
        }
    }

    fn collect_references(&mut self, file: &PathBuf, source: &str, expression: &Output<'_>) {
        fn visit_output(index: &mut SymbolIndex, file: &PathBuf, source: &str, output: &Output<'_>) {
            visit(index, file, source, output.1.as_ref(), output.0.start..output.0.end);
        }

        fn visit_outputs(index: &mut SymbolIndex, file: &PathBuf, source: &str, outputs: &[Output<'_>]) {
            for output in outputs {
                visit_output(index, file, source, output);
            }
        }

        /// Owner and member sites of `owner::member` spanning `span`.
        fn visit_path(
            index: &mut SymbolIndex,
            file: &Path,
            source: &str,
            owner: &str,
            member: &str,
            span: &Range<usize>,
        ) {
            let Some(member_range) = path_member_range(source, member, span) else {
                return;
            };
            let leaf = owner.rsplit("::").next().unwrap_or(owner).trim();
            let before = source[span.start..member_range.start].trim_end();
            let Some(head) = before.strip_suffix("::").map(str::trim_end) else {
                return;
            };
            if !leaf.is_empty() && head.ends_with(leaf) {
                let end = span.start + head.len();
                let mut push = |name: String, range: Range<usize>, part: PathPart| {
                    index.references.entry(name.clone()).or_default().push(RefSite {
                        name,
                        file: file.to_path_buf(),
                        range,
                        def_id: None,
                        path: Some(part),
                    });
                };
                push(leaf.to_owned(), end - leaf.len()..end, PathPart::Owner);
                push(format!("{leaf}::{member}"), member_range, PathPart::Member);
            }
        }

        fn visit(
            index: &mut SymbolIndex,
            file: &PathBuf,
            source: &str,
            expression: &Expression<'_>,
            span: Range<usize>,
        ) {
            match expression {
                Expression::Identifier(name) => {
                    index
                        .references
                        .entry((*name).to_owned())
                        .or_default()
                        .push(RefSite {
                            name: (*name).to_owned(),
                            file: file.clone(),
                            range: span,
                            def_id: None,
                            path: None,
                        });
                }
                Expression::Integer(_)
                | Expression::Float(_)
                | Expression::String(_)
                | Expression::Bool(_)
                | Expression::Type(_)
                | Expression::TypeProjection { .. }
                | Expression::Default(_)
                | Expression::Use { .. }
                | Expression::ExternBlock { .. }
                | Expression::ExternStruct(_)
                | Expression::Break
                | Expression::Continue
                | Expression::AssocTypeDecl { .. } => {}
                Expression::Program(items)
                | Expression::Block(items)
                | Expression::Fragment(items)
                | Expression::Array(items)
                | Expression::Tuple(items)
                | Expression::If(items)
                | Expression::Declare(items)
                | Expression::Invoke(items) => visit_outputs(index, file, source, items),
                Expression::Noop(inner)
                | Expression::Module(_, inner)
                | Expression::Spread(inner)
                | Expression::Return(inner)
                | Expression::ImplicitReturn(inner)
                | Expression::Raise(inner)
                | Expression::Panic(inner)
                | Expression::Yield(inner)
                | Expression::YieldFrom(inner)
                | Expression::Try(inner)
                | Expression::TypeOf(inner)
                | Expression::Comptime(inner)
                | Expression::OptionalAccess(inner, _)
                | Expression::Negate(inner)
                | Expression::Not(inner)
                | Expression::LogicalNot(inner)
                | Expression::Positive(inner)
                | Expression::Expr(inner)
                | Expression::Group(inner)
                | Expression::ExprStatement(inner)
                | Expression::Statement(inner)
                | Expression::Readonly(inner)
                | Expression::Dload(inner)
                | Expression::Done(inner)
                | Expression::Member(inner)
                | Expression::Access(inner, _)
                | Expression::Method(_, inner)
                | Expression::NamedArg(_, inner)
                | Expression::Branch(None, inner)
                | Expression::Adjust { target: inner, .. } => visit_output(index, file, source, inner),
                Expression::Resume(a, b) => {
                    visit_output(index, file, source, a);
                    if let Some(b) = b {
                        visit_output(index, file, source, b);
                    }
                }
                Expression::Coalesce(a, b)
                | Expression::Cast(a, b)
                | Expression::CompoundAssign(a, _, b)
                | Expression::Add(a, b)
                | Expression::Sub(a, b)
                | Expression::Mul(a, b)
                | Expression::Div(a, b)
                | Expression::Mod(a, b)
                | Expression::Pow(a, b)
                | Expression::Shl(a, b)
                | Expression::Shr(a, b)
                | Expression::Xor(a, b)
                | Expression::And(a, b)
                | Expression::BitAnd(a, b)
                | Expression::Or(a, b)
                | Expression::BitOr(a, b)
                | Expression::Eq(a, b)
                | Expression::Neq(a, b)
                | Expression::Leq(a, b)
                | Expression::Geq(a, b)
                | Expression::Le(a, b)
                | Expression::Gt(a, b)
                | Expression::Assignment(a, b)
                | Expression::Branch(Some(a), b) => {
                    visit_output(index, file, source, a);
                    visit_output(index, file, source, b);
                }
                Expression::Range { start, end, .. } => {
                    visit_output(index, file, source, start);
                    visit_output(index, file, source, end);
                }
                Expression::Dict(fields) => {
                    for field in fields {
                        visit_output(index, file, source, &field.value);
                    }
                }
                Expression::Index(target, index_expr) => {
                    visit_output(index, file, source, target);
                    if let Some(index_expr) = index_expr {
                        visit_output(index, file, source, index_expr);
                    }
                }
                Expression::StaticDecl { ty, init, .. } => {
                    if let Some(ty) = ty {
                        visit_output(index, file, source, ty);
                    }
                    visit_output(index, file, source, init);
                }
                Expression::Argument { ty, .. } => {
                    if let Some(ty) = ty {
                        visit_output(index, file, source, ty);
                    }
                }
                Expression::TypeFn { params, ret, .. } => {
                    visit_output(index, file, source, params);
                    visit_output(index, file, source, ret);
                }
                Expression::TypeApp { args, .. } => visit_outputs(index, file, source, args),
                Expression::MacroCall { name, args } => {
                    let start = span.start;
                    let end = (start + name.len()).min(span.end);
                    index.references.entry((*name).to_owned()).or_default().push(RefSite {
                        name: (*name).to_owned(),
                        file: file.clone(),
                        range: start..end,
                        def_id: None,
                        path: None,
                    });
                    visit_outputs(index, file, source, args);
                }
                Expression::Quote { parts, .. } => {
                    for part in parts {
                        if let parser::ast::QuotePart::Splice(e) | parser::ast::QuotePart::Repeat { list: e, .. } = part {
                            visit_output(index, file, source, e);
                        }
                    }
                }
                Expression::AttrDecl {
                    args,
                    returns,
                    body,
                    ..
                }
                | Expression::DeriveDecl {
                    args,
                    returns,
                    body,
                    ..
                }
                | Expression::FnMacroDecl {
                    args,
                    returns,
                    body,
                    ..
                } => {
                    visit_output(index, file, source, args);
                    if let Some(returns) = returns {
                        visit_output(index, file, source, returns);
                    }
                    visit_output(index, file, source, body);
                }
                Expression::Function { args, body, .. } => {
                    visit_output(index, file, source, args);
                    if let Some(body) = body {
                        visit_output(index, file, source, body);
                    }
                }
                Expression::Call { name, args } => {
                    visit_output(index, file, source, name);
                    if let Some(args) = args {
                        visit_outputs(index, file, source, args);
                    }
                }
                Expression::Defer { body, .. } => visit_output(index, file, source, body),
                Expression::Lambda { args, body, .. } => {
                    visit_output(index, file, source, args);
                    visit_output(index, file, source, body);
                }
                Expression::Loop {
                    contracts: _,
                    identifier,
                    pattern: _,
                    iterable,
                    body,
                } => {
                    if let Some(identifier) = identifier {
                        visit_output(index, file, source, identifier);
                    }
                    visit_output(index, file, source, iterable);
                    visit_output(index, file, source, body);
                }
                Expression::Variable(_, init) => {
                    if let Some(init) = init {
                        visit_output(index, file, source, init);
                    }
                }
                Expression::Constant(name, init) => {
                    visit_output(index, file, source, name);
                    if let Some(init) = init {
                        visit_output(index, file, source, init);
                    }
                }
                Expression::LetDestructure { rhs, .. } => visit_output(index, file, source, rhs),
                Expression::Class { fields, .. } => visit_outputs(index, file, source, fields),
                Expression::Implementation { methods, .. }
                | Expression::TypeClass { methods, .. } => visit_outputs(index, file, source, methods),
                Expression::TypeClassImpl { args, methods, .. } => {
                    visit_outputs(index, file, source, args);
                    visit_outputs(index, file, source, methods);
                }
                Expression::Field { name, ty, init, .. } => {
                    visit_output(index, file, source, name);
                    visit_output(index, file, source, ty);
                    if let Some(init) = init {
                        visit_output(index, file, source, init);
                    }
                }
                Expression::Instantiate(name, args) => {
                    visit_output(index, file, source, name);
                    if let Some(args) = args {
                        visit_outputs(index, file, source, args);
                    }
                }
                Expression::TypeAlias { ty, .. } | Expression::Forall { ty, .. } => {
                    visit_output(index, file, source, ty)
                }
                Expression::TestCase { name, body } => {
                    visit_output(index, file, source, name);
                    visit_output(index, file, source, body);
                }
                // Expanded before this runs; its body is the expansion program's.
                Expression::ComptimeBlock(_) => {}
                Expression::EnumDecl { variants, .. } => visit_outputs(index, file, source, variants),
                Expression::EnumVariant { payload, .. } => match payload {
                    EnumVariantPayload::Unit => {}
                    EnumVariantPayload::Tuple(parts) => visit_outputs(index, file, source, parts),
                    EnumVariantPayload::Record(fields) => {
                        for field in fields {
                            visit_output(index, file, source, &field.value);
                        }
                    }
                },
                Expression::QualifiedAccess { owner, member } => {
                    visit_path(index, file, source, owner, member, &span)
                }
                Expression::Construct {
                    enum_name,
                    variant_name,
                    fields,
                } => {
                    visit_path(index, file, source, enum_name, variant_name, &span);
                    match fields {
                        EnumConstructPayload::Unit => {}
                        EnumConstructPayload::Tuple(parts) => visit_outputs(index, file, source, parts),
                        EnumConstructPayload::Record(fields) => {
                            for field in fields {
                                visit_output(index, file, source, &field.value);
                            }
                        }
                    }
                }
                Expression::Match { scrutinee, arms } => {
                    visit_output(index, file, source, scrutinee);
                    for arm in arms {
                        visit_output(index, file, source, &arm.body);
                    }
                }
                Expression::IfLet {
                    scrutinee,
                    then_arm,
                    else_arm,
                } => {
                    visit_output(index, file, source, scrutinee);
                    visit_output(index, file, source, &then_arm.body);
                    visit_output(index, file, source, &else_arm.body);
                }
                Expression::WhileLet {
                    scrutinee,
                    then_arm,
                    on_miss,
                } => {
                    visit_output(index, file, source, scrutinee);
                    visit_output(index, file, source, &then_arm.body);
                    visit_output(index, file, source, &on_miss.body);
                }
                Expression::AssocTypeDef { ty, .. } => visit_output(index, file, source, ty),
            }
        }

        visit(
            self,
            file,
            source,
            expression.1.as_ref(),
            expression.0.start..expression.0.end,
        );
    }

    /// Attach checker [`DefId`]s for names resolved in this file (locals + `use`).
    pub fn bind_def_ids(&mut self, locals: &HashMap<String, DefId>) {
        for defs in self.definitions.values_mut() {
            for def in defs {
                if def.def_id.is_none() {
                    def.def_id = locals.get(&def.name).copied();
                }
            }
        }
        for sites in self.references.values_mut() {
            for site in sites {
                if site.def_id.is_none() {
                    site.def_id = locals.get(&site.name).copied();
                }
            }
        }
    }

    pub fn def_id_for_name(&self, name: &str) -> Option<DefId> {
        self.definitions(name)
            .iter()
            .find_map(|d| d.def_id)
            .or_else(|| self.references(name).iter().find_map(|s| s.def_id))
    }
}

/// Name span inside `item`, without a whole-file `source.find(name)`.
fn name_range_in(source: &str, name: &str, item: &Range<usize>, prefer_last: bool) -> Range<usize> {
    if let Some(r) = slice_offset_in_source(source, name)
        && r.start >= item.start && r.end <= item.end
            && !prefer_last {
                return r;
            }
    let Some(slice) = source.get(item.clone()) else {
        return item.start..item.start;
    };
    let found = if prefer_last {
        slice.rfind(name)
    } else {
        slice.find(name)
    };
    match found {
        Some(off) => {
            let start = item.start + off;
            start..start + name.len()
        }
        None => item.start..item.start.saturating_add(name.len()).min(item.end),
    }
}

/// Span of the path's last segment `member` inside `span`: the parser's
/// slice when it points into `source`, else the last textual hit.
fn path_member_range(source: &str, member: &str, span: &Range<usize>) -> Option<Range<usize>> {
    if member.is_empty() {
        return None;
    }
    if let Some(r) = slice_offset_in_source(source, member)
        && r.start >= span.start
        && r.end <= span.end
    {
        return Some(r);
    }
    let slice = source.get(span.clone())?;
    // `Owner::member(args)`: the member precedes the payload.
    let head = slice.find(['(', '{']).map_or(slice, |cut| &slice[..cut]);
    let off = head.rfind(member)?;
    Some(span.start + off..span.start + off + member.len())
}

fn slice_offset_in_source(source: &str, name: &str) -> Option<Range<usize>> {
    let src = source.as_ptr() as usize;
    let n = name.as_ptr() as usize;
    if n >= src && n + name.len() <= src + source.len() {
        let start = n - src;
        Some(start..start + name.len())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn index(source: &str) -> SymbolIndex {
        SymbolIndex::from_source(PathBuf::from("test.hy"), source)
    }

    #[test]
    fn indexes_top_level_definition_kinds() {
        let source = "\
use io::stdout as out;
type Id = int;
static let hits = 0;
enum Color { Red, Green }
class Point { pub x: int, pub y: int }
fn add(int a, int b) -> int { return a + b; }
";
        let idx = index(source);

        let out = idx.definitions("out");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, SymbolKind::Namespace);

        let id = idx.definitions("Id");
        assert_eq!(id.len(), 1);
        assert_eq!(id[0].kind, SymbolKind::TypeAlias);

        let hits = idx.definitions("hits");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, SymbolKind::Variable);

        let color = idx.definitions("Color");
        assert_eq!(color.len(), 1);
        assert_eq!(color[0].kind, SymbolKind::Enum);

        let point = idx.definitions("Point");
        assert_eq!(point.len(), 1);
        assert_eq!(point[0].kind, SymbolKind::Class);

        let add = idx.definitions("add");
        assert_eq!(add.len(), 1);
        assert_eq!(add[0].kind, SymbolKind::Function);
        assert_eq!(&source[add[0].name_range.clone()], "add");
    }

    #[test]
    fn indexes_macro_and_derive_declarations_and_calls() {
        let source = "\
/// Double.
macro twice(Expr e) -> Code { return quote expr { ${e} * 2 }; }
derive Answer(TypeDecl t) -> Code { return quote items { }; }
fn main() {
    let x = twice!(1 + 2);
}
";
        let idx = index(source);
        let twice = idx.definitions("twice");
        assert_eq!(twice.len(), 1);
        assert_eq!(twice[0].kind, SymbolKind::Macro);
        assert_eq!(&source[twice[0].name_range.clone()], "twice");

        let answer = idx.definitions("Answer");
        assert_eq!(answer.len(), 1);
        assert_eq!(answer[0].kind, SymbolKind::Macro);

        let refs = idx.references("twice");
        assert!(
            refs.iter().any(|site| &source[site.range.clone()] == "twice"),
            "expected twice! call site, got {refs:?}"
        );
    }

    #[test]
    fn indexes_call_identifier_references() {
        let source = "\
fn fib(int n) -> int {
    return fib(n - 1);
}
fn main() {
    fib(10);
    return;
}
";
        let idx = index(source);
        assert_eq!(idx.definitions("fib").len(), 1);
        let refs = idx.references("fib");
        assert!(
            refs.len() >= 2,
            "expected recursive + main call sites, got {refs:?}"
        );
        for site in refs {
            assert_eq!(&source[site.range.clone()], "fib");
        }
    }

    #[test]
    fn parse_failure_yields_empty_index() {
        let idx = index("fn {{{");
        assert!(idx.all_definitions().next().is_none());
        assert!(idx.definitions("anything").is_empty());
        assert!(idx.references("anything").is_empty());
    }

    #[test]
    fn use_without_alias_keeps_imported_name() {
        let idx = index("use io::stdout;\nfn main() { return; }\n");
        let defs = idx.definitions("stdout");
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].kind, SymbolKind::Namespace);
    }

    #[test]
    fn use_alias_name_range_skips_path_substring() {
        let source = "use io::stdout as out;\nfn main() { return; }\n";
        let idx = index(source);
        let defs = idx.definitions("out");
        assert_eq!(defs.len(), 1);
        assert_eq!(&source[defs[0].name_range.clone()], "out");
        assert!(
            defs[0].name_range.start > source.find("stdout").expect("path"),
            "alias span must not point at the `out` inside `stdout`"
        );
    }

    #[test]
    fn all_reference_sites_flattens_every_name() {
        let source = "\
fn fib(int n) -> int {
    return fib(n - 1);
}
fn main() {
    fib(10);
    return;
}
";
        let idx = index(source);
        let per_name = idx.references("fib").len() + idx.references("n").len();
        let flattened: Vec<_> = idx.all_reference_sites().collect();
        assert!(
            flattened.len() >= per_name,
            "flattened sites should include every named reference"
        );
        assert_eq!(
            flattened.iter().filter(|site| site.name == "fib").count(),
            idx.references("fib").len()
        );
        assert_eq!(
            flattened.iter().filter(|site| site.name == "n").count(),
            idx.references("n").len()
        );
        assert!(
            !flattened.is_empty(),
            "expected reference sites from recursive + main call"
        );
    }

    const TRAITS: &str = "\
trait Area<T> { fn size(T s) -> int {} }
trait Volume<T> { fn size(T s) -> int {} }
class Sq { pub w: int, }
impl Area for Sq { pub fn size(Sq s) -> int { return s.w; } }
impl Volume for Sq { pub fn size(Sq s) -> int { return s.w * 2; } }
fn main() { let a = Area::size(new Sq(1)); }
";

    #[test]
    fn indexes_traits_and_their_methods_by_trait() {
        let idx = index(TRAITS);
        let area = idx.definitions("Area");
        assert_eq!(area.len(), 1);
        assert_eq!(area[0].kind, SymbolKind::Trait);
        assert_eq!(&TRAITS[area[0].name_range.clone()], "Area");

        let area_size = idx.definitions("Area::size");
        let volume_size = idx.definitions("Volume::size");
        assert_eq!(area_size.len(), 1, "{area_size:?}");
        assert_eq!(volume_size.len(), 1, "{volume_size:?}");
        assert_eq!(area_size[0].kind, SymbolKind::Method);
        assert_eq!(&TRAITS[area_size[0].name_range.clone()], "size");
        assert_eq!(area_size[0].name_range.start, TRAITS.find("fn size").unwrap() + 3);
        assert_eq!(volume_size[0].name_range.start, TRAITS.rfind("fn size(T").unwrap() + 3);
        assert!(
            idx.definitions("size").is_empty(),
            "trait methods are not bare-name definitions"
        );
    }

    #[test]
    fn qualified_call_records_owner_and_member_sites() {
        let idx = index(TRAITS);
        let call = TRAITS.find("Area::size(").unwrap();
        let owner = idx.references("Area");
        assert!(
            owner.iter().any(|site| site.range == (call..call + 4) && site.path == Some(PathPart::Owner)),
            "expected the `Area` owner site, got {owner:?}"
        );
        let member = idx.references("Area::size");
        assert_eq!(member.len(), 1, "{member:?}");
        assert_eq!(member[0].range, call + 6..call + 10);
        assert_eq!(member[0].path, Some(PathPart::Member));
        assert!(idx.references("Volume::size").is_empty());
        assert!(
            idx.references("size").iter().all(|site| site.range != member[0].range),
            "the member is not a bare `size` reference"
        );
    }
}
