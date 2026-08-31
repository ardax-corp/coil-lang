use std::{
    collections::HashMap,
    ops::Range,
    path::PathBuf,
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
        index.collect_references(&file, &root);
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

    fn collect_definitions(&mut self, file: &PathBuf, source: &str, expression: &Output<'_>) {
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
                Expression::Use { name, alias, .. } => (
                    alias.as_deref().unwrap_or(name),
                    SymbolKind::Namespace,
                ),
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
                file: file.clone(),
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

    fn collect_references(&mut self, file: &PathBuf, expression: &Output<'_>) {
        fn visit_output(index: &mut SymbolIndex, file: &PathBuf, output: &Output<'_>) {
            visit(index, file, output.1.as_ref(), output.0.start..output.0.end);
        }

        fn visit_outputs(index: &mut SymbolIndex, file: &PathBuf, outputs: &[Output<'_>]) {
            for output in outputs {
                visit_output(index, file, output);
            }
        }

        fn visit(
            index: &mut SymbolIndex,
            file: &PathBuf,
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
                        });
                }
                Expression::Integer(_)
                | Expression::Float(_)
                | Expression::String(_)
                | Expression::Bool(_)
                | Expression::Type(_)
                | Expression::TypeProjection { .. }
                | Expression::Comment(_)
                | Expression::Default(_)
                | Expression::QualifiedAccess { .. }
                | Expression::Use { .. }
                | Expression::ExternBlock { .. }
                | Expression::ExternStruct(_)
                | Expression::Break
                | Expression::Continue
                | Expression::AssocTypeDecl { .. } => {}
                Expression::Program(items)
                | Expression::Block(items)
                | Expression::Fragment(items)
                | Expression::List(items)
                | Expression::Array(items)
                | Expression::Tuple(items)
                | Expression::If(items)
                | Expression::Declare(items)
                | Expression::Invoke(items) => visit_outputs(index, file, items),
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
                | Expression::Adjust { target: inner, .. } => visit_output(index, file, inner),
                Expression::Resume(a, b) => {
                    visit_output(index, file, a);
                    if let Some(b) = b {
                        visit_output(index, file, b);
                    }
                }
                Expression::Coalesce(a, b)
                | Expression::Cast(a, b)
                | Expression::TypeFun(a, b)
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
                    visit_output(index, file, a);
                    visit_output(index, file, b);
                }
                Expression::Range { start, end, .. } => {
                    visit_output(index, file, start);
                    visit_output(index, file, end);
                }
                Expression::Dict(fields) => {
                    for field in fields {
                        visit_output(index, file, &field.value);
                    }
                }
                Expression::Index(target, index_expr) => {
                    visit_output(index, file, target);
                    if let Some(index_expr) = index_expr {
                        visit_output(index, file, index_expr);
                    }
                }
                Expression::StaticDecl { ty, init, .. } => {
                    if let Some(ty) = ty {
                        visit_output(index, file, ty);
                    }
                    visit_output(index, file, init);
                }
                Expression::Argument { ty, .. } => {
                    if let Some(ty) = ty {
                        visit_output(index, file, ty);
                    }
                }
                Expression::TypeFnSig { params, ret } => {
                    visit_output(index, file, params);
                    visit_output(index, file, ret);
                }
                Expression::TypeApp { args, .. } => visit_outputs(index, file, args),
                Expression::AttrDecl {
                    args,
                    returns,
                    body,
                    ..
                } => {
                    visit_output(index, file, args);
                    if let Some(returns) = returns {
                        visit_output(index, file, returns);
                    }
                    visit_output(index, file, body);
                }
                Expression::Function { args, body, .. } => {
                    visit_output(index, file, args);
                    if let Some(body) = body {
                        visit_output(index, file, body);
                    }
                }
                Expression::Call { name, args } => {
                    visit_output(index, file, name);
                    if let Some(args) = args {
                        visit_outputs(index, file, args);
                    }
                }
                Expression::Defer { body, .. } => visit_output(index, file, body),
                Expression::Lambda { args, body, .. } => {
                    visit_output(index, file, args);
                    visit_output(index, file, body);
                }
                Expression::For {
                    init,
                    cond,
                    step,
                    body,
                } => {
                    if let Some(init) = init {
                        visit_output(index, file, init);
                    }
                    visit_output(index, file, cond);
                    if let Some(step) = step {
                        visit_output(index, file, step);
                    }
                    visit_output(index, file, body);
                }
                Expression::Loop {
                    identifier,
                    iterable,
                    body,
                } => {
                    if let Some(identifier) = identifier {
                        visit_output(index, file, identifier);
                    }
                    visit_output(index, file, iterable);
                    visit_output(index, file, body);
                }
                Expression::Variable(_, init) => {
                    if let Some(init) = init {
                        visit_output(index, file, init);
                    }
                }
                Expression::Constant(name, init) => {
                    visit_output(index, file, name);
                    if let Some(init) = init {
                        visit_output(index, file, init);
                    }
                }
                Expression::LetDestructure { rhs, .. } => visit_output(index, file, rhs),
                Expression::Class { fields, .. } => visit_outputs(index, file, fields),
                Expression::Implementation { methods, .. }
                | Expression::TypeClass { methods, .. } => visit_outputs(index, file, methods),
                Expression::TypeClassImpl { args, methods, .. } => {
                    visit_outputs(index, file, args);
                    visit_outputs(index, file, methods);
                }
                Expression::Field { name, ty, init, .. } => {
                    visit_output(index, file, name);
                    visit_output(index, file, ty);
                    if let Some(init) = init {
                        visit_output(index, file, init);
                    }
                }
                Expression::Instantiate(name, args) => {
                    visit_output(index, file, name);
                    if let Some(args) = args {
                        visit_outputs(index, file, args);
                    }
                }
                Expression::TypeAlias { ty, .. } | Expression::Forall { ty, .. } => {
                    visit_output(index, file, ty)
                }
                Expression::TestCase { name, body } => {
                    visit_output(index, file, name);
                    visit_output(index, file, body);
                }
                Expression::EnumDecl { variants, .. } => visit_outputs(index, file, variants),
                Expression::EnumVariant { payload, .. } => match payload {
                    EnumVariantPayload::Unit => {}
                    EnumVariantPayload::Tuple(parts) => visit_outputs(index, file, parts),
                    EnumVariantPayload::Record(fields) => {
                        for field in fields {
                            visit_output(index, file, &field.value);
                        }
                    }
                },
                Expression::Construct { fields, .. } => match fields {
                    EnumConstructPayload::Unit => {}
                    EnumConstructPayload::Tuple(parts) => visit_outputs(index, file, parts),
                    EnumConstructPayload::Record(fields) => {
                        for field in fields {
                            visit_output(index, file, &field.value);
                        }
                    }
                },
                Expression::Match { scrutinee, arms } => {
                    visit_output(index, file, scrutinee);
                    for arm in arms {
                        visit_output(index, file, &arm.body);
                    }
                }
                Expression::AssocTypeDef { ty, .. } => visit_output(index, file, ty),
            }
        }

        visit(self, file, expression.1.as_ref(), expression.0.start..expression.0.end);
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
    if let Some(r) = slice_offset_in_source(source, name) {
        if r.start >= item.start && r.end <= item.end {
            if !prefer_last {
                return r;
            }
        }
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
}
