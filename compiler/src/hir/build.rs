//! AST + checker facts to HIR ([`build_module`]).
//!
//! Runs after `check_program` on the same AST. Types come from the
//! [`TypedSidecar`] (by span, then by [`NodeId`]); layouts from
//! [`super::layout::of`]. Sugar is removed here:
//!
//! - `e?` is a `Match` whose miss arm returns the `None` / `Err`.
//! - `a ?? b` is a `Match` with `b` in the miss arm.
//! - `x op= e` and `x++` are `Assign` of a `Bin`.
//! - `while c { b }` is `Loop { if c { b } else { break } }`;
//!   `while let` and `if let` are `Match`.
//! - `raise e` is `Return(Make Err e)`, and in a Result-mode function a bare
//!   `return v` is `Return(Make Ok v)`.
//!
//! A construct the builder does not cover yet becomes
//! [`HirKind::Unsupported`]; [`super::check::problems`] reports them.

use std::collections::HashMap;

use parser::ast::{
    AdjustOp, AssignOp, EnumConstructPayload, Expression, LetPattern, MatchArm, Output, Pattern,
    PatternPayload,
};

use super::layout::{self, Layout};
use super::{
    BinOp, BodyKind, Builtin, Callee, HirArm, HirBody, HirExpr, HirFlags, HirId, HirKind,
    HirLocal, HirModule, HirPat, HirPatFields, IndexKind, Lit, LocalId, LocalKind, MakeKind, Span,
    UnOp,
};
use crate::typechecking::infer::{Checker, TypedSidecar};
use crate::typechecking::def_id::DefId;
use crate::typechecking::ty::{
    self as coil_ty, Ty, is_option_ty, option_inner, result_ok_err, result_ty, strip_readonly,
};

/// Build HIR for every body in `ast` (a checked module's `Program`).
/// `module` is the namespace path codegen compiles it under (`""` for the
/// entry file); it prefixes body names and finds the checker's schemes.
pub fn build_module(checker: &Checker, sidecar: &TypedSidecar, module_path: &str, ast: &Output<'_>) -> HirModule {
    let mut module = HirModule::default();
    let mut cx = Cx {
        checker,
        sidecar,
        module: &mut module,
    };
    let mut top = BodyBuilder::new(&join(module_path, "<top>"), BodyKind::TopLevel, span_of(ast));
    let mut top_stmts = Vec::new();
    cx.items(ast, module_path, &mut top, &mut top_stmts);
    if !top_stmts.is_empty() {
        let root = top.push(
            HirKind::Block {
                stmts: top_stmts,
                tail: None,
            },
            None,
            span_of(ast),
            None,
        );
        top.body.root = Some(root);
        let body = top.finish();
        module.bodies.insert(0, body);
        // Lambdas name their body by index; the top body now comes first.
        for body in &mut module.bodies {
            for expr in &mut body.exprs {
                if let HirKind::Lambda { body } = &mut expr.kind {
                    *body += 1;
                }
            }
        }
    }
    module
}

fn declared_effects(e: &parser::ast::EffectDecl<'_>) -> super::DeclaredEffects {
    super::DeclaredEffects {
        names: e.uses.iter().map(|n| n.to_string()).collect(),
        text: e.to_string(),
        span: (e.span.start, e.span.end),
    }
}

fn span_of(node: &Output<'_>) -> Span {
    (node.0.start, node.0.end)
}

struct Cx<'c, 'm> {
    checker: &'c Checker,
    sidecar: &'c TypedSidecar,
    module: &'m mut HirModule,
}

/// One body under construction, plus its lexical scopes.
struct BodyBuilder {
    body: HirBody,
    scopes: Vec<HashMap<String, LocalId>>,
    /// Enclosing body's scopes, for lambda captures (innermost last).
    outer: Vec<HashMap<String, LocalId>>,
    /// Result-mode with an `Ok` payload that is itself a `Result`: a
    /// returned `Result::Ok(..)` is the payload, so it is wrapped too.
    ok_is_result: bool,
}

impl BodyBuilder {
    fn new(name: &str, kind: BodyKind, span: Span) -> Self {
        Self {
            body: HirBody {
                name: name.to_string(),
                kind,
                span,
                params: Vec::new(),
                ret: None,
                ret_layout: Layout::Word,
                result_mode: false,
                is_coro: false,
                is_generic: false,
                captures: Vec::new(),
                declared: None,
                locals: Vec::new(),
                exprs: Vec::new(),
                root: None,
            },
            scopes: vec![HashMap::new()],
            outer: Vec::new(),
            ok_is_result: false,
        }
    }

    fn push(
        &mut self,
        kind: HirKind,
        ty: Option<Ty>,
        span: Span,
        node: Option<crate::typechecking::id::NodeId>,
    ) -> HirId {
        let id = HirId(self.body.exprs.len() as u32);
        self.body.exprs.push(HirExpr {
            kind,
            ty,
            layout: Layout::Word,
            span,
            node,
            flags: HirFlags::default(),
        });
        id
    }

    fn local(&mut self, name: &str, ty: Option<Ty>, kind: LocalKind) -> LocalId {
        let id = LocalId(self.body.locals.len() as u32);
        self.body.locals.push(HirLocal {
            name: name.to_string(),
            ty,
            kind,
            captured: false,
        });
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_string(), id);
        }
        id
    }

    fn temp(&mut self, name: &str, ty: Option<Ty>) -> LocalId {
        let id = LocalId(self.body.locals.len() as u32);
        self.body.locals.push(HirLocal {
            name: name.to_string(),
            ty,
            kind: LocalKind::Temp,
            captured: false,
        });
        id
    }

    fn lookup(&mut self, name: &str) -> Option<LocalId> {
        for scope in self.scopes.iter().rev() {
            if let Some(id) = scope.get(name) {
                return Some(*id);
            }
        }
        // A name of an enclosing body: capture it.
        for scope in self.outer.iter().rev() {
            if let Some(outer_id) = scope.get(name).copied() {
                let inner = LocalId(self.body.locals.len() as u32);
                self.body.locals.push(HirLocal {
                    name: name.to_string(),
                    ty: None,
                    kind: LocalKind::Capture,
                    captured: false,
                });
                self.body.captures.push((outer_id, inner));
                self.scopes[0].insert(name.to_string(), inner);
                return Some(inner);
            }
        }
        None
    }

    fn finish(self) -> HirBody {
        self.body
    }
}

/// Peel `Expr` / `Group` / `Statement` / one-item `Fragment` wrappers.
fn peel<'a, 'e>(node: &'a Output<'e>) -> &'a Output<'e> {
    match node.1.as_ref() {
        Expression::Expr(inner) | Expression::Group(inner) | Expression::Statement(inner) => {
            peel(inner)
        }
        Expression::Fragment(items) if items.len() == 1 => peel(&items[0]),
        _ => node,
    }
}

fn is_result_construct(node: &Output<'_>, ok_is_result: bool) -> bool {
    match peel(node).1.as_ref() {
        Expression::Construct {
            enum_name,
            variant_name,
            ..
        } => {
            (*enum_name == common::BUILTIN_RESULT_ENUM || enum_name.ends_with("::Result"))
                && (*variant_name == "Err" || (*variant_name == "Ok" && !ok_is_result))
        }
        Expression::Call { name, .. } => {
            matches!(peel(name).1.as_ref(), Expression::Identifier(n) if *n == "Err" || (*n == "Ok" && !ok_is_result))
        }
        _ => false,
    }
}

impl<'c, 'm> Cx<'c, 'm> {
    // ----- facts -------------------------------------------------------

    fn ty_of(&self, node: &Output<'_>) -> Option<Ty> {
        self.sidecar
            .ty_at_span(node.0.start, node.0.end)
            .cloned()
            .or_else(|| {
                let id = self.checker.id_table().id_of_output(node)?;
                self.sidecar.ty(id).cloned().or_else(|| self.checker.lookup_at(id))
            })
    }

    fn node_id(&self, node: &Output<'_>) -> Option<crate::typechecking::id::NodeId> {
        self.checker.id_table().id_of_output(node)
    }

    /// Push a node built from `node`, stamping its type, layout and flags.
    fn emit(&self, b: &mut BodyBuilder, node: &Output<'_>, kind: HirKind) -> HirId {
        let ty = self.ty_of(node);
        self.emit_ty(b, node, kind, ty)
    }

    fn emit_ty(&self, b: &mut BodyBuilder, node: &Output<'_>, kind: HirKind, ty: Option<Ty>) -> HirId {
        let id = self.node_id(node);
        let hir = b.push(kind, ty, span_of(node), id);
        self.stamp(b, hir);
        if let Some(id) = id {
            let flags = &mut b.body.exprs[hir.0 as usize].flags;
            if self.sidecar.is_frame_local(id) {
                flags.insert(HirFlags::FRAME_LOCAL);
            }
            if self.sidecar.is_frame_local_last_use(id) {
                flags.insert(HirFlags::LAST_USE);
            }
            if self.sidecar.is_in_bounds_index(id) {
                flags.insert(HirFlags::IN_BOUNDS);
            }
            if self.sidecar.is_nonneg_expr(id) {
                flags.insert(HirFlags::NONNEG);
            }
        }
        hir
    }

    /// Push a desugaring node with no AST node of its own.
    fn synth(&self, b: &mut BodyBuilder, span: Span, kind: HirKind, ty: Option<Ty>) -> HirId {
        let hir = b.push(kind, ty, span, None);
        self.stamp(b, hir);
        hir
    }

    fn stamp(&self, b: &mut BodyBuilder, hir: HirId) {
        let expr = &mut b.body.exprs[hir.0 as usize];
        if let Some(ty) = &expr.ty {
            expr.layout = layout::of_resolved(self.checker, ty);
        }
    }

    /// Give an untyped node (an assignment place) `from`'s type.
    fn fill_ty(&self, b: &mut BodyBuilder, id: HirId, from: HirId) {
        if b.body.exprs[id.0 as usize].ty.is_none() {
            b.body.exprs[id.0 as usize].ty = self.ty_at(b, from);
            self.stamp(b, id);
        }
    }

    /// The element type an `index` node reads, from its base's type.
    fn element_ty(&self, b: &BodyBuilder, id: HirId) -> Option<Ty> {
        let HirKind::Index { base, .. } = b.body.exprs[id.0 as usize].kind else {
            return None;
        };
        let base_ty = self.ty_at(b, base)?;
        match strip_readonly(&crate::typechecking::subst::apply_ty_prune(self.checker.subst(), &base_ty)) {
            Ty::Array { element, .. } => Some(element.as_ref().clone()),
            Ty::App(head, args) if matches!(head.as_ref(), Ty::Con(n) if n == common::BUILTIN_VEC_TYPE) => args.first().cloned(),
            _ => None,
        }
    }

    fn ty_at(&self, b: &BodyBuilder, id: HirId) -> Option<Ty> {
        b.body.exprs[id.0 as usize].ty.clone()
    }

    fn unsupported(&self, b: &mut BodyBuilder, node: &Output<'_>, what: &'static str) -> HirId {
        self.emit(b, node, HirKind::Unsupported(what))
    }

    // ----- items -------------------------------------------------------

    /// Walk module-level items, building one body per function; top-level
    /// statements go to `stmts` of the `<top>` body.
    fn items(&mut self, node: &Output<'_>, prefix: &str, top: &mut BodyBuilder, stmts: &mut Vec<HirId>) {
        match node.1.as_ref() {
            Expression::Fragment(items)
                if matches!(
                    items.first().map(|i| i.1.as_ref()),
                    Some(Expression::Variable(..) | Expression::Constant(..))
                ) =>
            {
                let id = self.expr(top, node);
                stmts.push(id);
            }
            Expression::Program(items) | Expression::Block(items) | Expression::Fragment(items) => {
                for item in items {
                    self.items(item, prefix, top, stmts);
                }
            }
            Expression::Expr(inner) | Expression::Statement(inner) | Expression::Group(inner) => {
                self.items(inner, prefix, top, stmts)
            }
            Expression::Module(name, body) => {
                let prefix = join(prefix, name);
                self.items(body, &prefix, top, stmts);
            }
            Expression::Function { name, .. } => {
                let full = join(prefix, name);
                let keys = vec![full.clone(), name.to_string()];
                self.function(node, &full, BodyKind::Function, None, &keys);
            }
            Expression::Method(_, inner) => self.items(inner, prefix, top, stmts),
            Expression::Implementation { owner, methods, .. } => {
                for method in methods {
                    let Some(m) = fn_name(method) else { continue };
                    let full = join(&join(prefix, owner), m);
                    let keys = vec![full.clone(), format!("{owner}::{m}")];
                    self.function(fn_node(method), &full, BodyKind::Method, Some(owner), &keys);
                }
            }
            Expression::TypeClassImpl {
                class,
                args,
                methods,
                ..
            } => {
                let owner = args
                    .first()
                    .map(|a| a.1.to_string())
                    .unwrap_or_default();
                let span = node.0.into_range();
                let instances = &self.checker.generics().instances;
                let instance = instances
                    .iter()
                    .find(|inst| inst.class == *class && inst.range == span)
                    .or_else(|| {
                        // Derive-generated impls carry a synthetic span.
                        let mut by_head = instances.iter().filter(|inst| {
                            inst.class == *class
                                && inst.args.first().is_some_and(|a| head_name(a) == head_str(&owner))
                        });
                        let first = by_head.next();
                        if by_head.next().is_some() { None } else { first }
                    });
                let path = join(prefix, &format!("{class} for {owner}"));
                for method in methods {
                    let Some(m) = fn_name(method) else { continue };
                    let mut keys = Vec::new();
                    if let Some(fqn) = instance.and_then(|i| i.method_fqns.get(m)) {
                        keys.push(fqn.clone());
                    }
                    let full = join(&path, m);
                    self.function(fn_node(method), &full, BodyKind::Method, None, &keys);
                }
            }
            Expression::TypeClass { name, methods, .. } => {
                for method in methods {
                    if let Expression::Function { name: m, effects, .. } = fn_node(method).1.as_ref() {
                        self.module.trait_effects.push(super::TraitEffects {
                            trait_name: name.to_string(),
                            method: m.to_string(),
                            declared: effects.as_ref().map(declared_effects),
                        });
                    }
                }
                // Default method bodies.
                for method in methods {
                    let Some(m) = fn_name(method) else { continue };
                    let keys = vec![crate::typechecking::generics::Generics::default_method_fqn(name, m)];
                    let full = join(&join(prefix, name), m);
                    self.function(fn_node(method), &full, BodyKind::Method, None, &keys);
                }
            }
            Expression::TestCase { name, body } => {
                let label = match peel(name).1.as_ref() {
                    Expression::String(s) => format!("test {s:?}"),
                    _ => "test".to_string(),
                };
                let mut tb = BodyBuilder::new(&label, BodyKind::Test, span_of(node));
                tb.body.result_mode = true;
                tb.body.ret = Some(result_ty(coil_ty::unit(), coil_ty::string()));
                let root = self.expr(&mut tb, body);
                self.implicit_ok_return(&mut tb, root);
                tb.body.root = Some(root);
                self.module.bodies.push(tb.finish());
            }
            // Declarations with no body.
            Expression::Class { .. }
            | Expression::EnumDecl { .. }
            | Expression::TypeAlias { .. }
            | Expression::Use { .. }
            | Expression::ExternBlock { .. }
            | Expression::ExternStruct(_)
            | Expression::AttrDecl { .. }
            | Expression::DeriveDecl { .. }
            | Expression::FnMacroDecl { .. }
            | Expression::Noop(_) => {}
            _ => {
                let id = self.expr(top, node);
                stmts.push(id);
            }
        }
    }

    /// Build one function body. `owner` is set for inherent methods, whose
    /// `self` is implicit; `keys` are the checker's names for its scheme.
    fn function(&mut self, node: &Output<'_>, full: &str, kind: BodyKind, owner: Option<&str>, keys: &[String]) {
        let Expression::Function {
            is_coro,
            is_static,
            type_params,
            args,
            effects,
            body,
            ..
        } = node.1.as_ref()
        else {
            return;
        };
        // A signature without a body (trait declaration, `fn f();`).
        let Some(body) = body else { return };
        let mut b = BodyBuilder::new(full, kind, span_of(node));
        b.body.is_coro = *is_coro;
        b.body.declared = effects.as_ref().map(declared_effects);
        b.body.is_generic = !type_params.is_empty();
        let ret = keys.iter().find_map(|k| self.checker.fn_return_ty(k));
        b.body.ret_layout = ret
            .as_ref()
            .map_or(Layout::Word, |ty| layout::of_resolved(self.checker, ty));
        b.body.ret = ret;
        // Inherent methods take their result mode from the bare method name,
        // as the AST's `compile_function_decl_into` reads it.
        let bare = full.rsplit("::").next().unwrap_or(full).to_string();
        let mode_keys = if owner.is_some() { std::slice::from_ref(&bare) } else { keys };
        b.body.result_mode = mode_keys.iter().any(|k| self.checker.fn_is_result_mode(k));
        b.ok_is_result = mode_keys.iter().any(|k| self.checker.fn_result_ok_is_result(k));
        if let Some(owner) = owner
            && !is_static
        {
            let id = b.local("self", Some(Ty::Con(owner.to_string())), LocalKind::Param);
            b.body.params.push(id);
        }
        let param_tys = keys.iter().find_map(|k| self.checker.fn_param_tys(k));
        self.params(&mut b, args, param_tys.as_deref());
        let root = self.expr(&mut b, body);
        self.implicit_ok_return(&mut b, root);
        b.body.root = Some(root);
        // In a generic class's shared method body `self` is the one object
        // word for every instance: its reads take the bare class type, not
        // the scheme's open `C<T>`.
        if let Some(owner) = owner
            && !is_static
            && crate::hir::lower::is_generic_class(self.checker, owner)
            && let Some(&this) = b.body.params.first()
        {
            let key = self.checker.resolve_class_key(owner).unwrap_or_else(|| owner.to_string());
            for e in &mut b.body.exprs {
                if matches!(e.kind, HirKind::Local(l) if l == this) {
                    e.ty = Some(Ty::Con(key.clone()));
                }
            }
        }
        self.module.bodies.push(b.finish());
    }

    /// A Result-mode body with a unit `Ok` that can fall off its end returns
    /// `Ok(())` there; make that return explicit.
    fn implicit_ok_return(&self, b: &mut BodyBuilder, root: HirId) {
        if !b.body.result_mode {
            return;
        }
        let Some((ok, _)) = b.body.ret.as_ref().and_then(result_ok_err) else {
            return;
        };
        if !layout::is_unit(&ok) {
            return;
        }
        let HirKind::Block { stmts, tail: None } = &b.body.exprs[root.0 as usize].kind else {
            return;
        };
        let diverges = stmts.last().is_some_and(|last| {
            matches!(b.body.exprs[last.0 as usize].ty.as_ref(), Some(Ty::Never))
        });
        if diverges {
            return;
        }
        let span = (b.body.span.1, b.body.span.1);
        let unit = self.synth(b, span, HirKind::Lit(Lit::Unit), Some(ok));
        let ret = b.body.ret.clone();
        let ok = self.synth_variant(b, span, common::BUILTIN_RESULT_ENUM, "Ok", vec![unit], ret);
        let r = self.synth(b, span, HirKind::Return(Some(ok)), Some(Ty::Never));
        if let HirKind::Block { stmts, .. } = &mut b.body.exprs[root.0 as usize].kind {
            stmts.push(r);
        }
    }

    fn params(&mut self, b: &mut BodyBuilder, args: &Output<'_>, tys: Option<&[Ty]>) {
        let items: Vec<&Output<'_>> = match args.1.as_ref() {
            Expression::Fragment(items) => items.iter().collect(),
            _ => vec![args],
        };
        let skip = b.body.params.len();
        for (i, item) in items.into_iter().enumerate() {
            if let Expression::Argument { name, .. } = item.1.as_ref() {
                let ty = tys
                    .and_then(|t| t.get(skip + i).or_else(|| t.get(i)))
                    .cloned()
                    .filter(|t| !matches!(t, Ty::Con(n) if n == coil_ty::UNIT))
                    .or_else(|| self.ty_of(item));
                let id = b.local(name, ty, LocalKind::Param);
                b.body.params.push(id);
            }
        }
    }

    // ----- expressions -------------------------------------------------

    fn exprs(&mut self, b: &mut BodyBuilder, items: &[Output<'_>]) -> Vec<HirId> {
        items.iter().map(|item| self.expr(b, item)).collect()
    }

    fn expr(&mut self, b: &mut BodyBuilder, node: &Output<'_>) -> HirId {
        use Expression as E;
        match node.1.as_ref() {
            E::Integer(n) => self.emit_ty(b, node, HirKind::Lit(Lit::Int(*n)), self.ty_of(node).or(Some(coil_ty::int()))),
            E::Float(f) => self.emit_ty(b, node, HirKind::Lit(Lit::Float(*f)), Some(coil_ty::float())),
            E::Bool(v) => self.emit_ty(b, node, HirKind::Lit(Lit::Bool(*v)), Some(coil_ty::boolean())),
            E::String(s) => {
                let ty = self.ty_of(node).or(Some(coil_ty::string()));
                // A literal typed `[byte; N]` is its bytes, an array literal
                // (the AST keeps it in frame slots, as `let a = [..]`).
                if let Some(Ty::Array {
                    element,
                    length: coil_ty::ArrayLength::Static(n),
                }) = ty.as_ref().map(strip_readonly)
                    && matches!(strip_readonly(element), Ty::Con(e) if e == coil_ty::BYTE)
                {
                    let bytes = crate::codegen::unescape_coil_string(s).into_bytes();
                    if bytes.len() == *n {
                        let span = span_of(node);
                        let args = bytes
                            .iter()
                            .map(|&byte| {
                                let item = b.push(HirKind::Lit(Lit::Int(i64::from(byte))), Some(Ty::Con(coil_ty::BYTE.into())), span, None);
                                self.stamp(b, item);
                                item
                            })
                            .collect();
                        return self.emit_ty(b, node, HirKind::Make { kind: MakeKind::Array, args }, ty);
                    }
                }
                self.emit_ty(b, node, HirKind::Lit(Lit::Str(s.to_string())), ty)
            }
            E::Noop(_) => self.emit_ty(b, node, HirKind::Lit(Lit::Unit), Some(coil_ty::unit())),

            E::Expr(inner) | E::Group(inner) | E::Statement(inner) => self.expr(b, inner),
            E::ExprStatement(inner) => self.expr(b, inner),
            E::NamedArg(name, value) => {
                let value = self.expr(b, value);
                let ty = self.ty_at(b, value);
                self.emit_ty(b, node, HirKind::Named { name: name.to_string(), value }, ty)
            }
            E::Spread(inner) => {
                let inner = self.expr(b, inner);
                self.emit(b, node, HirKind::Spread(inner))
            }

            E::Identifier(name) => self.ident(b, node, name),
            E::QualifiedAccess { owner, member } => {
                if let Some(tag) = self.checker.tag_for(owner, member) {
                    self.emit(
                        b,
                        node,
                        HirKind::Make {
                            kind: MakeKind::Variant {
                                enum_name: owner.to_string(),
                                variant: member.to_string(),
                                tag: Some(tag),
                                fields: None,
                            },
                            args: Vec::new(),
                        },
                    )
                } else {
                    let name = format!("{owner}::{member}");
                    let def = self.node_id(node).and_then(|id| self.sidecar.def_id(id));
                    self.global(b, node, name, def)
                }
            }

            E::Block(items) => self.block(b, node, items, true),
            E::Program(items) => self.block(b, node, items, false),
            E::Fragment(items) => self.fragment(b, node, items),

            E::Variable(name, _) => {
                // A bare `let x;` (the init, if any, is the next fragment item).
                let ty = self.local_ty(node, name);
                let local = b.local(name, ty, LocalKind::Let);
                self.emit_ty(b, node, HirKind::Let { local, init: None }, Some(coil_ty::unit()))
            }
            E::Constant(name, _) => {
                let n = match name.1.as_ref() {
                    E::Identifier(n) => n.to_string(),
                    _ => "<const>".to_string(),
                };
                let ty = self.ty_of(name);
                let local = b.local(&n, ty, LocalKind::Const);
                self.emit_ty(b, node, HirKind::Let { local, init: None }, Some(coil_ty::unit()))
            }
            E::LetDestructure { pattern, rhs } => {
                let init = self.expr(b, rhs);
                let rhs_ty = self.ty_at(b, init);
                let pat = self.let_pattern(b, pattern, rhs_ty.as_ref());
                self.emit_ty(b, node, HirKind::LetPat { pat, init }, Some(coil_ty::unit()))
            }
            E::StaticDecl { name, init, .. } => {
                let ty = self.ty_of(init);
                let local = b.local(name, ty, LocalKind::Const);
                let init = self.expr(b, init);
                self.emit_ty(b, node, HirKind::Let { local, init: Some(init) }, Some(coil_ty::unit()))
            }

            E::Assignment(target, value) => {
                if let E::Index(base, None) = peel(target).1.as_ref() {
                    let base = self.expr(b, base);
                    let value = self.expr(b, value);
                    return self.emit(b, node, HirKind::Append { base, value });
                }
                let value = self.expr(b, value);
                let place = self.expr(b, target);
                self.fill_ty(b, place, value);
                self.emit(b, node, HirKind::Assign { place, value })
            }
            E::CompoundAssign(target, op, value) => self.compound_assign(b, node, target, *op, value),
            E::Adjust { op, prefix, target } => self.adjust(b, node, *op, *prefix, target),

            E::Add(l, r) => self.binary(b, node, l, r, "+"),
            E::Sub(l, r) => self.binary(b, node, l, r, "-"),
            E::Mul(l, r) => self.binary(b, node, l, r, "*"),
            E::Div(l, r) => self.binary(b, node, l, r, "/"),
            E::Mod(l, r) => self.binary(b, node, l, r, "%"),
            E::Pow(l, r) => self.binary(b, node, l, r, "**"),
            E::Shl(l, r) => self.binary(b, node, l, r, "<<"),
            E::Shr(l, r) => self.binary(b, node, l, r, ">>"),
            E::Xor(l, r) => self.binary(b, node, l, r, "^"),
            E::BitAnd(l, r) => self.binary(b, node, l, r, "&"),
            E::BitOr(l, r) => self.binary(b, node, l, r, "|"),
            E::Eq(l, r) => self.binary(b, node, l, r, "=="),
            E::Neq(l, r) => self.binary(b, node, l, r, "!="),
            E::Le(l, r) => self.binary(b, node, l, r, "<"),
            E::Leq(l, r) => self.binary(b, node, l, r, "<="),
            E::Gt(l, r) => self.binary(b, node, l, r, ">"),
            E::Geq(l, r) => self.binary(b, node, l, r, ">="),
            E::And(l, r) | E::Or(l, r) => {
                let and = matches!(node.1.as_ref(), E::And(..));
                let lhs = self.expr(b, l);
                let rhs = self.expr(b, r);
                self.emit_ty(b, node, HirKind::Logic { and, lhs, rhs }, Some(coil_ty::boolean()))
            }
            E::Negate(e) => self.unary(b, node, e, UnOp::Neg),
            E::Not(e) => self.unary(b, node, e, UnOp::BitNot),
            E::LogicalNot(e) => self.unary(b, node, e, UnOp::Not),
            E::Positive(e) => self.expr(b, e),
            E::Cast(value, _) => {
                let value = self.expr(b, value);
                self.emit(b, node, HirKind::Cast { value })
            }

            E::Range { start, end, inclusive } => {
                let args = vec![self.expr(b, start), self.expr(b, end)];
                self.emit(b, node, HirKind::Make { kind: MakeKind::Range { inclusive: *inclusive }, args })
            }
            E::List(items) => {
                let args = self.exprs(b, items);
                self.emit(b, node, HirKind::Make { kind: MakeKind::List, args })
            }
            E::Array(items) => {
                let args = self.exprs(b, items);
                self.emit(b, node, HirKind::Make { kind: MakeKind::Array, args })
            }
            E::Tuple(items) => {
                let args = self.exprs(b, items);
                self.emit(b, node, HirKind::Make { kind: MakeKind::Tuple, args })
            }
            E::Dict(fields) => {
                let names = fields.iter().map(|f| f.name.to_string()).collect();
                let args = fields.iter().map(|f| self.expr(b, &f.value)).collect();
                self.emit(b, node, HirKind::Make { kind: MakeKind::Record(names), args })
            }
            // `C::f(args)` on a class is a static method call (a bare
            // `C::f` names a static field first, as in codegen).
            E::Construct {
                enum_name,
                variant_name,
                fields: fields @ (EnumConstructPayload::Unit | EnumConstructPayload::Tuple(_)),
            } if self.static_call(enum_name, variant_name, matches!(fields, EnumConstructPayload::Unit), node) => {
                let id = self.node_id(node);
                let overload = id.and_then(|i| self.sidecar.overload(i)).map(|o| o.candidate_id);
                let def = id.and_then(|i| self.sidecar.def_id(i));
                let args = match fields {
                    EnumConstructPayload::Tuple(items) => self.exprs(b, items),
                    _ => Vec::new(),
                };
                self.emit(
                    b,
                    node,
                    HirKind::Call {
                        callee: Callee::Named { name: format!("{enum_name}::{variant_name}"), def, overload },
                        args,
                    },
                )
            }
            // `Class::field` of a class `static`: a static read.
            E::Construct {
                enum_name,
                variant_name,
                fields: EnumConstructPayload::Unit,
            } if self.class_static(enum_name, variant_name) => {
                let def = self.node_id(node).and_then(|id| self.sidecar.def_id(id));
                self.global(b, node, format!("{enum_name}::{variant_name}"), def)
            }
            E::Construct { enum_name, variant_name, fields } => {
                let (args, names) = match fields {
                    EnumConstructPayload::Unit => (Vec::new(), None),
                    EnumConstructPayload::Tuple(items) => (self.exprs(b, items), None),
                    EnumConstructPayload::Record(fields) => {
                        let src: Vec<HirId> = fields.iter().map(|f| self.expr(b, &f.value)).collect();
                        let names: Vec<String> = fields.iter().map(|f| f.name.to_string()).collect();
                        return self.record_variant(b, node, enum_name, variant_name, src, names);
                    }
                };
                self.make_variant(b, node, enum_name, variant_name, args, names)
            }
            E::Instantiate(class, args) => {
                let name = match peel(class).1.as_ref() {
                    E::Identifier(n) | E::Type(n) => n.to_string(),
                    other => other.to_string(),
                };
                let args = args.as_ref().map_or_else(Vec::new, |a| self.exprs(b, a));
                self.emit(b, node, HirKind::Make { kind: MakeKind::Class(name), args })
            }

            E::Access(base, field) => {
                let base = self.expr(b, base);
                self.emit(b, node, HirKind::Field { base, name: field.to_string() })
            }
            E::OptionalAccess(base, field) => self.optional_access(b, node, base, field),
            E::Index(base, Some(index)) => {
                let base_id = self.expr(b, base);
                let index = self.expr(b, index);
                let kind = index_kind(self.ty_at(b, base_id).as_ref());
                self.emit(b, node, HirKind::Index { base: base_id, index, kind })
            }
            E::Index(_, None) => self.unsupported(b, node, "append outside assignment"),

            E::Call { name, args } => self.call(b, node, name, args.as_deref().unwrap_or(&[])),

            E::If(branches) => self.if_chain(b, node, branches),
            E::Branch(cond, body) => match cond {
                Some(cond) => {
                    let cond = self.expr(b, cond);
                    let then = self.expr(b, body);
                    self.emit(b, node, HirKind::If { cond, then, els: None })
                }
                None => self.expr(b, body),
            },
            E::Loop { identifier, pattern, iterable, body } => {
                self.loop_(b, node, identifier.as_ref(), pattern.as_ref(), iterable, body)
            }
            E::Break => self.emit_ty(b, node, HirKind::Break, Some(coil_ty::never())),
            E::Continue => self.emit_ty(b, node, HirKind::Continue, Some(coil_ty::never())),
            E::Return(value) | E::ImplicitReturn(value) => self.return_(b, node, value),
            E::Raise(err) => {
                let err = self.expr(b, err);
                let ok = b.body.ret.as_ref().and_then(result_ok_err).map(|(ok, _)| ok);
                let err_ty = self.ty_at(b, err);
                let ty = match (ok, err_ty) {
                    (Some(ok), Some(e)) => Some(result_ty(ok, e)),
                    _ => b.body.ret.clone(),
                };
                let make = self.synth_variant(b, span_of(node), common::BUILTIN_RESULT_ENUM, "Err", vec![err], ty);
                self.emit_ty(b, node, HirKind::Return(Some(make)), Some(coil_ty::never()))
            }
            E::Panic(msg) => {
                let msg = self.expr(b, msg);
                self.emit_ty(b, node, HirKind::Builtin { op: Builtin::Panic, args: vec![msg] }, Some(coil_ty::never()))
            }
            E::Try(inner) => self.try_(b, node, inner),
            E::Coalesce(lhs, rhs) => self.coalesce(b, node, lhs, rhs),

            E::Match { scrutinee, arms } => {
                let arms: Vec<&MatchArm<'_>> = arms.iter().collect();
                self.match_(b, node, scrutinee, &arms)
            }
            E::IfLet { scrutinee, then_arm, else_arm } => {
                self.match_(b, node, scrutinee, &[then_arm, else_arm])
            }
            E::WhileLet { scrutinee, then_arm, on_miss } => {
                let m = self.match_(b, node, scrutinee, &[then_arm, on_miss]);
                let span = span_of(node);
                let body = self.synth(b, span, HirKind::Block { stmts: vec![m], tail: None }, Some(coil_ty::unit()));
                self.emit_ty(b, node, HirKind::Loop { body }, Some(coil_ty::unit()))
            }

            E::Lambda { args, body, .. } => self.lambda(b, node, args, body),
            E::Defer { captures, body } => {
                let captures = captures.iter().map(|c| b.lookup(c)).collect();
                let body = self.expr(b, body);
                self.emit_ty(b, node, HirKind::Defer { captures, body }, Some(coil_ty::unit()))
            }
            E::Yield(value) | E::YieldFrom(value) => {
                let from = matches!(node.1.as_ref(), E::YieldFrom(_));
                let value = self.expr(b, value);
                self.emit(b, node, HirKind::Yield { value, from })
            }
            E::Resume(handle, value) => {
                let handle = self.expr(b, handle);
                let value = value.as_ref().map(|v| self.expr(b, v));
                self.emit(b, node, HirKind::Resume { handle, value })
            }

            E::TypeOf(inner) => self.builtin(b, node, Builtin::TypeOf, std::slice::from_ref(inner)),
            E::Dload(inner) => self.builtin(b, node, Builtin::Dload, std::slice::from_ref(inner)),
            E::Done(inner) => self.builtin(b, node, Builtin::Done, std::slice::from_ref(inner)),
            E::Readonly(inner) => self.builtin(b, node, Builtin::Readonly, std::slice::from_ref(inner)),
            E::Declare(args) => self.builtin(b, node, Builtin::Declare, args),
            E::Invoke(args) => self.builtin(b, node, Builtin::Invoke, args),
            E::Default(_) => self.builtin(b, node, Builtin::Default, &[]),

            // Items nested in a body (local functions, impls) build their own
            // bodies; the statement itself is a unit.
            E::Function { name, .. } => {
                let full = join(&b.body.name, name);
                let keys = vec![full.clone(), name.to_string()];
                self.function(node, &full, BodyKind::Function, None, &keys);
                self.emit_ty(b, node, HirKind::Lit(Lit::Unit), Some(coil_ty::unit()))
            }
            E::Class { .. } | E::EnumDecl { .. } | E::TypeAlias { .. } | E::Use { .. } => {
                self.emit_ty(b, node, HirKind::Lit(Lit::Unit), Some(coil_ty::unit()))
            }

            E::Member(_) => self.unsupported(b, node, "member"),
            E::MacroCall { .. } => self.unsupported(b, node, "unexpanded macro call"),
            E::Quote { .. } => self.unsupported(b, node, "quote"),
            E::Module(..) => self.unsupported(b, node, "nested module"),
            _ => self.unsupported(b, node, "declaration in expression position"),
        }
    }

    fn builtin(&mut self, b: &mut BodyBuilder, node: &Output<'_>, op: Builtin, args: &[Output<'_>]) -> HirId {
        let args = self.exprs(b, args);
        self.emit(b, node, HirKind::Builtin { op, args })
    }

    /// Type of a `let` local: the node's own or, for `let x = …` fragments,
    /// the init's (filled at the fragment).
    fn local_ty(&self, node: &Output<'_>, _name: &str) -> Option<Ty> {
        self.ty_of(node).filter(|ty| !matches!(ty, Ty::Con(n) if n == coil_ty::UNIT))
    }

    fn ident(&mut self, b: &mut BodyBuilder, node: &Output<'_>, name: &str) -> HirId {
        if let Some(local) = b.lookup(name) {
            let ty = self.ty_of(node);
            let slot = &mut b.body.locals[local.0 as usize];
            if slot.ty.is_none() {
                slot.ty.clone_from(&ty);
            }
            // An assignment target has no type of its own: use the local's.
            let ty = ty.or_else(|| slot.ty.clone());
            return self.emit_ty(b, node, HirKind::Local(local), ty);
        }
        if name == "None" {
            return self.make_variant(b, node, common::BUILTIN_OPTION_ENUM, "None", Vec::new(), None);
        }
        // A bare unit variant (`Empty` for `ParseError::Empty`).
        let (start, end) = span_of(node);
        if let Some((enum_name, variant)) = self.checker.bare_construct_at(start, end) {
            let (enum_name, variant) = (enum_name.clone(), variant.clone());
            return self.make_variant(b, node, &enum_name, &variant, Vec::new(), None);
        }
        let def = self.node_id(node).and_then(|id| self.sidecar.def_id(id));
        self.global(b, node, name.to_string(), def)
    }

    /// A global read. An assignment or `++` target has no type of its own:
    /// a static takes its declared one.
    fn global(&self, b: &mut BodyBuilder, node: &Output<'_>, name: String, def: Option<DefId>) -> HirId {
        let ty = self.ty_of(node).or_else(|| self.static_ty(&name));
        self.emit_ty(b, node, HirKind::Global { name, def }, ty)
    }

    fn static_ty(&self, name: &str) -> Option<Ty> {
        let slot = match name.rsplit_once("::") {
            Some((owner, member)) if self.checker.is_class(owner) => {
                let key = self.checker.resolve_class_key(owner).unwrap_or_else(|| owner.to_string());
                self.checker.static_slot_index(&format!("{key}::{member}"))
            }
            _ => self
                .checker
                .static_slot_index(name)
                .or_else(|| self.checker.static_slot_for_module_name(name)),
        }?;
        self.checker.static_slot_ty(slot).cloned()
    }

    fn block(&mut self, b: &mut BodyBuilder, node: &Output<'_>, items: &[Output<'_>], scoped: bool) -> HirId {
        if scoped {
            b.scopes.push(HashMap::new());
        }
        let mut stmts = Vec::with_capacity(items.len());
        for item in items {
            stmts.push(self.expr(b, item));
        }
        if scoped {
            b.scopes.pop();
        }
        // The value of a block is its last expression unless it is a
        // `;`-terminated statement.
        let tail = match items.last().map(|n| n.1.as_ref()) {
            Some(Expression::ExprStatement(_)) | None => None,
            Some(_) => self.pop_tail(b, &mut stmts),
        };
        self.emit(b, node, HirKind::Block { stmts, tail })
    }

    fn fragment(&mut self, b: &mut BodyBuilder, node: &Output<'_>, items: &[Output<'_>]) -> HirId {
        // `let x[: T] = init` and `const x = init` parse as `[decl, init]`.
        if let [decl, init] = items {
            match decl.1.as_ref() {
                Expression::Variable(name, _) => {
                    let init = self.expr(b, init);
                    let ty = self.local_ty(decl, name).or_else(|| self.ty_at(b, init));
                    let local = b.local(name, ty, LocalKind::Let);
                    return self.emit_ty(b, node, HirKind::Let { local, init: Some(init) }, Some(coil_ty::unit()));
                }
                Expression::Constant(name, _) => {
                    let n = match name.1.as_ref() {
                        Expression::Identifier(n) => n.to_string(),
                        _ => "<const>".to_string(),
                    };
                    let init = self.expr(b, init);
                    let ty = self.ty_of(name).or_else(|| self.ty_at(b, init));
                    let local = b.local(&n, ty, LocalKind::Const);
                    return self.emit_ty(b, node, HirKind::Let { local, init: Some(init) }, Some(coil_ty::unit()));
                }
                _ => {}
            }
        }
        if let [one] = items {
            return self.expr(b, one);
        }
        let mut stmts = self.exprs(b, items);
        let tail = self.pop_tail(b, &mut stmts);
        self.emit(b, node, HirKind::Block { stmts, tail })
    }

    /// The last statement as the block's value, unless it is a binding.
    fn pop_tail(&self, b: &BodyBuilder, stmts: &mut Vec<HirId>) -> Option<HirId> {
        let last = *stmts.last()?;
        match b.body.exprs[last.0 as usize].kind {
            HirKind::Let { .. } | HirKind::LetPat { .. } | HirKind::Defer { .. } => None,
            _ => stmts.pop(),
        }
    }

    fn binary(&mut self, b: &mut BodyBuilder, node: &Output<'_>, l: &Output<'_>, r: &Output<'_>, op: &'static str) -> HirId {
        let lhs = self.expr(b, l);
        let rhs = self.expr(b, r);
        let op = resolve_bin(op, self.ty_at(b, lhs).as_ref(), self.ty_at(b, rhs).as_ref());
        let ty = self.ty_of(node).or_else(|| match op {
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => Some(coil_ty::boolean()),
            _ => self.ty_at(b, lhs),
        });
        self.emit_ty(b, node, HirKind::Bin { op, lhs, rhs }, ty)
    }

    fn unary(&mut self, b: &mut BodyBuilder, node: &Output<'_>, e: &Output<'_>, op: UnOp) -> HirId {
        let operand = self.expr(b, e);
        self.emit(b, node, HirKind::Un { op, operand })
    }

    /// `x op= e` as `x = x op e`. The place is built twice (read and write);
    /// lowering evaluates a place's base once.
    fn compound_assign(&mut self, b: &mut BodyBuilder, node: &Output<'_>, target: &Output<'_>, op: AssignOp, value: &Output<'_>) -> HirId {
        let sym = match op {
            AssignOp::Add => "+",
            AssignOp::Sub => "-",
            AssignOp::Mul => "*",
            AssignOp::Div => "/",
            AssignOp::Mod => "%",
            AssignOp::Pow => "**",
            AssignOp::Shl => "<<",
            AssignOp::Shr => ">>",
            AssignOp::BitAnd => "&",
            AssignOp::BitOr => "|",
            AssignOp::BitXor => "^",
        };
        let read = self.expr(b, target);
        let rhs = self.expr(b, value);
        let lhs_ty = self.ty_at(b, read);
        let bin_op = resolve_bin(sym, lhs_ty.as_ref(), self.ty_at(b, rhs).as_ref());
        let bin = self.synth(b, span_of(node), HirKind::Bin { op: bin_op, lhs: read, rhs }, lhs_ty);
        let place = self.expr(b, target);
        self.fill_ty(b, place, bin);
        let id = self.emit(b, node, HirKind::Assign { place, value: bin });
        b.body.exprs[id.0 as usize].flags.insert(HirFlags::COMPOUND);
        id
    }

    /// `x++` / `--x` as `x = x ± 1`, flagged `ADJUST` (and `PREFIX`): the
    /// node's value is the old or new `x`.
    fn adjust(&mut self, b: &mut BodyBuilder, node: &Output<'_>, op: AdjustOp, prefix: bool, target: &Output<'_>) -> HirId {
        let read = self.expr(b, target);
        // An element or field read may carry no type of its own; the
        // adjust's own value has the place's type.
        let ty = self.ty_at(b, read).or_else(|| self.ty_of(node)).or_else(|| self.element_ty(b, read));
        if b.body.exprs[read.0 as usize].ty.is_none() {
            b.body.exprs[read.0 as usize].ty = ty.clone();
            self.stamp(b, read);
        }
        let is_float = matches!(ty.as_ref().map(strip_readonly), Some(Ty::Con(n)) if n == coil_ty::FLOAT);
        let one = if is_float {
            self.synth(b, span_of(node), HirKind::Lit(Lit::Float(1.0)), Some(coil_ty::float()))
        } else {
            self.synth(b, span_of(node), HirKind::Lit(Lit::Int(1)), Some(coil_ty::int()))
        };
        let sym = match op {
            AdjustOp::Inc => "+",
            AdjustOp::Dec => "-",
        };
        let bin_op = resolve_bin(sym, ty.as_ref(), ty.as_ref());
        let bin = self.synth(b, span_of(node), HirKind::Bin { op: bin_op, lhs: read, rhs: one }, ty.clone());
        let place = self.expr(b, target);
        self.fill_ty(b, place, bin);
        let id = self.emit_ty(b, node, HirKind::Assign { place, value: bin }, ty);
        let flags = &mut b.body.exprs[id.0 as usize].flags;
        flags.insert(HirFlags::ADJUST);
        flags.insert(HirFlags::COMPOUND);
        if prefix {
            flags.insert(HirFlags::PREFIX);
        }
        id
    }

    fn call(&mut self, b: &mut BodyBuilder, node: &Output<'_>, name: &Output<'_>, args: &[Output<'_>]) -> HirId {
        use Expression as E;
        let callee_node = peel(name);
        match callee_node.1.as_ref() {
            E::Identifier(n) if b.lookup(n).is_none() => {
                if matches!(*n, "Some") {
                    let args = self.exprs(b, args);
                    return self.make_variant(b, node, common::BUILTIN_OPTION_ENUM, "Some", args, None);
                }
                if matches!(*n, "Ok" | "Err") {
                    let args = self.exprs(b, args);
                    return self.make_variant(b, node, common::BUILTIN_RESULT_ENUM, n, args, None);
                }
                // A bare variant constructor (`Bad(s)` for `ParseError::Bad`).
                let (start, end) = span_of(node);
                if let Some((enum_name, variant)) = self.checker.bare_construct_at(start, end) {
                    let (enum_name, variant) = (enum_name.clone(), variant.clone());
                    let args = self.exprs(b, args);
                    return self.make_variant(b, node, &enum_name, &variant, args, None);
                }
                let id = self.node_id(node);
                let def = self
                    .node_id(callee_node)
                    .and_then(|i| self.sidecar.def_id(i))
                    .or_else(|| id.and_then(|i| self.sidecar.def_id(i)));
                let overload = id.and_then(|i| self.sidecar.overload(i)).map(|o| o.candidate_id);
                let args = self.exprs(b, args);
                self.emit(
                    b,
                    node,
                    HirKind::Call {
                        callee: Callee::Named { name: n.to_string(), def, overload },
                        args,
                    },
                )
            }
            E::QualifiedAccess { owner, member } => {
                if self.checker.tag_for(owner, member).is_some() {
                    let args = self.exprs(b, args);
                    return self.make_variant(b, node, owner, member, args, None);
                }
                let id = self.node_id(node);
                let overload = id.and_then(|i| self.sidecar.overload(i)).map(|o| o.candidate_id);
                let def = self.node_id(callee_node).and_then(|i| self.sidecar.def_id(i));
                let args = self.exprs(b, args);
                self.emit(
                    b,
                    node,
                    HirKind::Call {
                        callee: Callee::Named { name: format!("{owner}::{member}"), def, overload },
                        args,
                    },
                )
            }
            E::Access(recv, method) => {
                let recv = self.expr(b, recv);
                let mut all = vec![recv];
                all.extend(self.exprs(b, args));
                self.emit(b, node, HirKind::Call { callee: Callee::Method { name: method.to_string() }, args: all })
            }
            _ => {
                let callee = self.expr(b, name);
                let args = self.exprs(b, args);
                self.emit(b, node, HirKind::Call { callee: Callee::Value(callee), args })
            }
        }
    }

    /// `Owner::member` in construct form calls a class's static method.
    /// `owner::member` names a static field of class `owner`.
    fn class_static(&self, owner: &str, member: &str) -> bool {
        if self.checker.tag_for(owner, member).is_some() || !self.checker.is_class(owner) {
            return false;
        }
        let key = self.checker.resolve_class_key(owner).unwrap_or_else(|| owner.to_string());
        self.checker.static_slot_index(&format!("{key}::{member}")).is_some()
    }

    fn static_call(&self, owner: &str, member: &str, bare: bool, node: &Output<'_>) -> bool {
        if self.checker.tag_for(owner, member).is_some() {
            return false;
        }
        // `T::m(..)` / `int::m(..)` on a non-class owner (no static fields,
        // so even the bare form): a static trait method the typechecker
        // dispatched through a bound or a ground instance.
        if !self.checker.is_class(owner) {
            let (start, end) = (node.0.start, node.0.end);
            return self.node_id(node).is_some_and(|id| {
                self.checker.bound_method_call_at(id).is_some() || self.checker.call_dicts_at(id).is_some()
            }) || self.checker.bound_method_call_span(start, end).is_some()
                || self.checker.call_dicts_span(start, end).is_some();
        }
        let key = self.checker.resolve_class_key(owner).unwrap_or_else(|| owner.to_string());
        !(bare && self.checker.static_slot_index(&format!("{key}::{member}")).is_some())
    }

    /// `E::V { f: a, .. }`: the payload is in declaration order, as the
    /// AST's construct emits it, whatever order the call site names the
    /// fields in. Shuffled arguments with effects still run in source order,
    /// through temps.
    fn record_variant(&mut self, b: &mut BodyBuilder, node: &Output<'_>, enum_name: &str, variant: &str, src: Vec<HirId>, names: Vec<String>) -> HirId {
        let decl = self.checker.payload_tys_for(enum_name, variant);
        let order: Option<Vec<usize>> = decl.iter().map(|(d, _)| names.iter().position(|n| n == d)).collect();
        let Some(order) = order.filter(|o| o.len() == src.len() && o.iter().enumerate().any(|(i, &j)| i != j)) else {
            return self.make_variant(b, node, enum_name, variant, src, Some(names));
        };
        let decl_names = order.iter().map(|&j| names[j].clone()).collect();
        let pure = src.iter().all(|&a| matches!(b.body.exprs[a.0 as usize].kind, HirKind::Lit(_) | HirKind::Local(_)));
        if pure {
            let args = order.iter().map(|&j| src[j]).collect();
            return self.make_variant(b, node, enum_name, variant, args, Some(decl_names));
        }
        let span = span_of(node);
        let mut stmts = Vec::with_capacity(src.len());
        let mut reads = Vec::with_capacity(src.len());
        for (&arg, name) in src.iter().zip(&names) {
            let ty = self.ty_at(b, arg);
            let t = b.temp(name, ty.clone());
            stmts.push(self.synth(b, span, HirKind::Let { local: t, init: Some(arg) }, Some(coil_ty::unit())));
            reads.push(self.synth(b, span, HirKind::Local(t), ty));
        }
        let args = order.iter().map(|&j| reads[j]).collect();
        let make = self.make_variant(b, node, enum_name, variant, args, Some(decl_names));
        let ty = self.ty_at(b, make);
        self.synth(b, span, HirKind::Block { stmts, tail: Some(make) }, ty)
    }

    fn make_variant(&mut self, b: &mut BodyBuilder, node: &Output<'_>, enum_name: &str, variant: &str, args: Vec<HirId>, fields: Option<Vec<String>>) -> HirId {
        let tag = self.checker.tag_for(enum_name, variant);
        self.emit(
            b,
            node,
            HirKind::Make {
                kind: MakeKind::Variant {
                    enum_name: enum_name.to_string(),
                    variant: variant.to_string(),
                    tag,
                    fields,
                },
                args,
            },
        )
    }

    fn synth_variant(&self, b: &mut BodyBuilder, span: Span, enum_name: &str, variant: &str, args: Vec<HirId>, ty: Option<Ty>) -> HirId {
        let tag = self.checker.tag_for(enum_name, variant);
        self.synth(
            b,
            span,
            HirKind::Make {
                kind: MakeKind::Variant {
                    enum_name: enum_name.to_string(),
                    variant: variant.to_string(),
                    tag,
                    fields: None,
                },
                args,
            },
            ty,
        )
    }

    fn if_chain(&mut self, b: &mut BodyBuilder, node: &Output<'_>, branches: &[Output<'_>]) -> HirId {
        // Build from the last branch backwards so each `else` nests.
        let chain_ty = self.ty_of(node);
        let mut els: Option<HirId> = None;
        for (i, branch) in branches.iter().enumerate().rev() {
            match branch.1.as_ref() {
                Expression::Branch(Some(cond), body) => {
                    b.scopes.push(HashMap::new());
                    let cond = self.expr(b, cond);
                    let then = self.expr(b, body);
                    b.scopes.pop();
                    let kind = HirKind::If { cond, then, els };
                    let at = if i == 0 { node } else { branch };
                    els = Some(self.emit_ty(b, at, kind, chain_ty.clone()));
                }
                Expression::Branch(None, body) => {
                    els = Some(self.expr(b, body));
                }
                _ => els = Some(self.expr(b, branch)),
            }
        }
        els.unwrap_or_else(|| self.emit_ty(b, node, HirKind::Lit(Lit::Unit), Some(coil_ty::unit())))
    }

    fn loop_(&mut self, b: &mut BodyBuilder, node: &Output<'_>, identifier: Option<&Output<'_>>, pattern: Option<&LetPattern<'_>>, iterable: &Output<'_>, body: &Output<'_>) -> HirId {
        let span = span_of(node);
        if identifier.is_none() && pattern.is_none() {
            // `while cond { body }` is `loop { if cond { body } else { break } }`.
            let cond = self.expr(b, iterable);
            b.scopes.push(HashMap::new());
            let then = self.expr(b, body);
            b.scopes.pop();
            let brk = self.synth(b, span, HirKind::Break, Some(coil_ty::never()));
            let test = self.synth(b, span, HirKind::If { cond, then, els: Some(brk) }, Some(coil_ty::unit()));
            let block = self.synth(b, span, HirKind::Block { stmts: vec![test], tail: None }, Some(coil_ty::unit()));
            return self.emit_ty(b, node, HirKind::Loop { body: block }, Some(coil_ty::unit()));
        }
        let iter = self.expr(b, iterable);
        let info = self
            .node_id(node)
            .and_then(|id| self.sidecar.for_in(id).cloned())
            .or_else(|| self.checker.for_in_info_span(node.0.start, node.0.end).cloned());
        let item_ty = info.as_ref().map(|i| i.item_ty.clone());
        b.scopes.push(HashMap::new());
        let pat = match (identifier, pattern) {
            (_, Some(p)) => self.let_pattern(b, p, item_ty.as_ref()),
            (Some(ident), None) => match peel(ident).1.as_ref() {
                Expression::Identifier(n) | Expression::Variable(n, _) => {
                    HirPat::Bind(b.local(n, item_ty.clone(), LocalKind::Pattern))
                }
                _ => HirPat::Wild,
            },
            (None, None) => HirPat::Wild,
        };
        // `for _ in xs` still steps an item: bind it to a temp.
        let pat = match pat {
            HirPat::Wild => HirPat::Bind(b.temp("item", item_ty.clone())),
            pat => pat,
        };
        let body = self.expr(b, body);
        b.scopes.pop();
        self.emit_ty(
            b,
            node,
            HirKind::ForIn { pat, iterable: iter, body, kind: info.map(|i| i.kind) },
            Some(coil_ty::unit()),
        )
    }

    fn return_(&mut self, b: &mut BodyBuilder, node: &Output<'_>, value: &Output<'_>) -> HirId {
        let is_unit = matches!(peel(value).1.as_ref(), Expression::Noop(_));
        let wrap = b.body.result_mode
            && !is_result_construct(value, b.ok_is_result)
            && !self.checker.returns_whole_result(value.0.start, value.0.end)
            && b.body.ret.as_ref().and_then(result_ok_err).is_some();
        // `return e?` (re-wrapped in Ok by result mode) and `return Ok(e?)` /
        // `return Some(e?)` of the function's own type return `e` as is, as
        // the AST's `expr_try_return_src` forwards the pair.
        if let Some(src) = self.try_forward_src(b, value, wrap) {
            let v = self.expr(b, src);
            return self.emit_ty(b, node, HirKind::Return(Some(v)), Some(coil_ty::never()));
        }
        let v = self.expr(b, value);
        let v = if wrap {
            let ret = b.body.ret.clone();
            let span = span_of(value);
            self.synth_variant(b, span, common::BUILTIN_RESULT_ENUM, "Ok", vec![v], ret)
        } else {
            v
        };
        let v = if is_unit && !wrap { None } else { Some(v) };
        self.emit_ty(b, node, HirKind::Return(v), Some(coil_ty::never()))
    }

    /// The `e` of a returned `e?` that is the identity: `e` has the
    /// function's own return type and the try's hit is re-wrapped in the
    /// same variant (`wrap` for result mode, or a written `Ok` / `Some`).
    fn try_forward_src<'a, 'e>(&self, b: &BodyBuilder, value: &'a Output<'e>, wrap: bool) -> Option<&'a Output<'e>> {
        if b.body.is_coro {
            return None;
        }
        let node = peel(value);
        let try_node = match node.1.as_ref() {
            Expression::Try(_) if wrap => node,
            Expression::Construct {
                enum_name,
                variant_name,
                fields: parser::ast::EnumConstructPayload::Tuple(args),
            } if args.len() == 1
                && ((common::is_builtin_result_enum(enum_name) && *variant_name == "Ok" && !b.ok_is_result)
                    || (common::is_builtin_option_enum(enum_name) && *variant_name == "Some")) =>
            {
                peel(&args[0])
            }
            _ => return None,
        };
        let Expression::Try(inner) = try_node.1.as_ref() else { return None };
        let span = span_of(try_node);
        if self.checker.test_try_at(span.0, span.1).is_some() {
            return None;
        }
        let ret = b.body.ret.as_ref().map(strip_readonly)?;
        let ty = self.ty_of(inner)?;
        (strip_readonly(&ty) == ret).then_some(inner)
    }

    /// `e?`: `match e { Some(x) / Ok(x) => x, miss => return miss }`.
    fn try_(&mut self, b: &mut BodyBuilder, node: &Output<'_>, inner: &Output<'_>) -> HirId {
        let span = span_of(node);
        let scrutinee = self.expr(b, inner);
        let sty = self.ty_at(b, scrutinee);
        let resolved = sty.as_ref().map(strip_readonly);
        let ok_ty = self.ty_of(node);
        let (enum_name, hit, miss) = match resolved {
            Some(t) if is_option_ty(t) => (common::BUILTIN_OPTION_ENUM, "Some", "None"),
            Some(t) if result_ok_err(t).is_some() => (common::BUILTIN_RESULT_ENUM, "Ok", "Err"),
            _ => return self.unsupported(b, node, "`?` on an unresolved type"),
        };
        let x = b.temp("ok", ok_ty.clone());
        let hit_pat = self.variant_pat(enum_name, hit, HirPatFields::Tuple(vec![HirPat::Bind(x)]));
        let hit_body = self.synth(b, span, HirKind::Local(x), ok_ty);
        // In a test body the miss arm fails the case with a message, as
        // the AST's `emit_test_try` (#628).
        let test_try = self.checker.test_try_at(span.0, span.1).cloned();
        // The miss arm re-wraps the error in the function's own return type.
        let (miss_pat, miss_val) = if let Some(kind) = test_try {
            use crate::typechecking::TestTry;
            let ret = miss_ret(b);
            let (pat, text) = match kind {
                TestTry::NoneValue => (
                    self.variant_pat(enum_name, miss, HirPatFields::Unit),
                    self.synth(b, span, HirKind::Lit(Lit::Str("`?` got None".into())), Some(coil_ty::string())),
                ),
                TestTry::ShowErr(err_ty) => {
                    let e = b.temp("err", Some(err_ty.clone()));
                    let read = self.synth(b, span, HirKind::Local(e), Some(err_ty));
                    let fmt = self.synth(b, span, HirKind::Lit(Lit::Str("`?` got Err(%v)".into())), Some(coil_ty::string()));
                    let callee = Callee::Named { name: "string::format".into(), def: None, overload: None };
                    let text = self.synth(b, span, HirKind::Call { callee, args: vec![fmt, read] }, Some(coil_ty::string()));
                    (self.variant_pat(enum_name, miss, HirPatFields::Tuple(vec![HirPat::Bind(e)])), text)
                }
            };
            (pat, self.synth_variant(b, span, common::BUILTIN_RESULT_ENUM, "Err", vec![text], ret))
        } else if enum_name == common::BUILTIN_OPTION_ENUM {
            let ret = miss_ret(b);
            (
                self.variant_pat(enum_name, miss, HirPatFields::Unit),
                self.synth_variant(b, span, enum_name, miss, Vec::new(), ret),
            )
        } else {
            let err_ty = resolved.and_then(result_ok_err).map(|(_, e)| e);
            let e = b.temp("err", err_ty.clone());
            let read = self.synth(b, span, HirKind::Local(e), err_ty);
            let ret = miss_ret(b);
            (
                self.variant_pat(enum_name, miss, HirPatFields::Tuple(vec![HirPat::Bind(e)])),
                self.synth_variant(b, span, enum_name, miss, vec![read], ret),
            )
        };
        let ret = self.synth(b, span, HirKind::Return(Some(miss_val)), Some(coil_ty::never()));
        let arms = vec![
            HirArm { pat: hit_pat, body: hit_body },
            HirArm { pat: miss_pat, body: ret },
        ];
        self.emit(b, node, HirKind::Match { scrutinee, arms })
    }

    /// `a ?? b`: `match a { Some(x) / Ok(x) => x, _ => b }`.
    fn coalesce(&mut self, b: &mut BodyBuilder, node: &Output<'_>, lhs: &Output<'_>, rhs: &Output<'_>) -> HirId {
        let span = span_of(node);
        let scrutinee = self.expr(b, lhs);
        let sty = self.ty_at(b, scrutinee);
        let (enum_name, hit, inner) = match sty.as_ref().map(strip_readonly) {
            Some(t) if is_option_ty(t) => (common::BUILTIN_OPTION_ENUM, "Some", option_inner(t)),
            Some(t) if result_ok_err(t).is_some() => {
                (common::BUILTIN_RESULT_ENUM, "Ok", result_ok_err(t).map(|(ok, _)| ok))
            }
            _ => return self.unsupported(b, node, "`??` on an unresolved type"),
        };
        let x = b.temp("val", inner.clone());
        let hit_pat = self.variant_pat(enum_name, hit, HirPatFields::Tuple(vec![HirPat::Bind(x)]));
        let hit_body = self.synth(b, span, HirKind::Local(x), inner);
        let default = self.expr(b, rhs);
        let arms = vec![
            HirArm { pat: hit_pat, body: hit_body },
            HirArm { pat: HirPat::Wild, body: default },
        ];
        self.emit(b, node, HirKind::Match { scrutinee, arms })
    }

    /// `e?.f`: `match e { Some(x) => Some(x.f), _ => None }`.
    fn optional_access(&mut self, b: &mut BodyBuilder, node: &Output<'_>, base: &Output<'_>, field: &str) -> HirId {
        let span = span_of(node);
        let scrutinee = self.expr(b, base);
        let inner = self.ty_at(b, scrutinee).as_ref().map(strip_readonly).and_then(option_inner);
        let out_ty = self.ty_of(node);
        let field_ty = out_ty.as_ref().map(strip_readonly).and_then(option_inner);
        let x = b.temp("some", inner.clone());
        let read = self.synth(b, span, HirKind::Local(x), inner);
        let get = self.synth(b, span, HirKind::Field { base: read, name: field.to_string() }, field_ty);
        let some = self.synth_variant(b, span, common::BUILTIN_OPTION_ENUM, "Some", vec![get], out_ty.clone());
        let none = self.synth_variant(b, span, common::BUILTIN_OPTION_ENUM, "None", Vec::new(), out_ty);
        let arms = vec![
            HirArm {
                pat: self.variant_pat(common::BUILTIN_OPTION_ENUM, "Some", HirPatFields::Tuple(vec![HirPat::Bind(x)])),
                body: some,
            },
            HirArm { pat: HirPat::Wild, body: none },
        ];
        self.emit(b, node, HirKind::Match { scrutinee, arms })
    }

    fn match_(&mut self, b: &mut BodyBuilder, node: &Output<'_>, scrutinee: &Output<'_>, arms: &[&MatchArm<'_>]) -> HirId {
        let scrutinee = self.expr(b, scrutinee);
        let sty = self.ty_at(b, scrutinee);
        let mut out = Vec::with_capacity(arms.len());
        for arm in arms {
            b.scopes.push(HashMap::new());
            let pat = self.pattern(b, &arm.pattern.1, sty.as_ref());
            let body = self.expr(b, &arm.body);
            b.scopes.pop();
            out.push(HirArm { pat, body });
        }
        let out = self.split_nested(b, span_of(node), sty.as_ref(), self.ty_of(node), out);
        self.emit(b, node, HirKind::Match { scrutinee, arms: out })
    }

    /// Regroup arms with nested payload patterns into one arm per outer
    /// variant over an inner `match` on the payload:
    /// `Ok(None) => a, Ok(Some(n)) => b, Err(e) => c` becomes
    /// `Ok(t) => match t { None => a, Some(n) => b }, Err(e) => c`.
    /// Outer variants are disjoint, so only arm order within a variant
    /// matters. A regrouped variant that is not exhaustive on its own takes
    /// the trailing catch-all as its last inner arm, sharing the body (the
    /// arm runs from either match), when that body is shareable; else the
    /// arms are left as is.
    fn split_nested(&self, b: &mut BodyBuilder, span: Span, sty: Option<&Ty>, ty: Option<Ty>, mut arms: Vec<HirArm>) -> Vec<HirArm> {
        if !arms.iter().any(|a| nested_payload(&a.pat).is_some_and(|p| !irrefutable(p))) {
            return arms;
        }
        if let Some(i) = arms.iter().position(|a| irrefutable(&a.pat)) {
            arms.truncate(i + 1);
        }
        let catch_all = arms.last().is_some_and(|a| irrefutable(&a.pat)).then(|| arms.pop()).flatten();
        // Arm indices per outer variant, in first-appearance order.
        let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
        for (i, arm) in arms.iter().enumerate() {
            let HirPat::Variant { variant, .. } = &arm.pat else {
                return restore(arms, catch_all);
            };
            match groups.iter_mut().find(|(v, _)| v == variant) {
                Some((_, idx)) => idx.push(i),
                None => groups.push((variant.clone(), vec![i])),
            }
        }
        let Some(sty) = sty else {
            return restore(arms, catch_all);
        };
        let mut plan = Vec::with_capacity(groups.len());
        for (variant, idx) in &groups {
            let first = &arms[idx[0]].pat;
            let split = idx.len() > 1 || nested_payload(first).is_some_and(|p| !irrefutable(p));
            if !split {
                plan.push(None);
                continue;
            }
            let HirPat::Variant { enum_name, .. } = first else {
                unreachable!("grouped arms are variants")
            };
            let Some(subs) = idx.iter().map(|&i| nested_payload(&arms[i].pat)).collect::<Option<Vec<_>>>() else {
                return restore(arms, catch_all);
            };
            let Some(field_ty) = self.variant_field_tys(enum_name, variant, Some(sty)).into_iter().next() else {
                return restore(arms, catch_all);
            };
            let fallthrough = !self.exhaustive(&subs, &field_ty);
            if fallthrough && !catch_all.as_ref().is_some_and(|c| shareable_catch_all(&b.body, c)) {
                return restore(arms, catch_all);
            }
            plan.push(Some((field_ty, fallthrough)));
        }
        let mut slots: Vec<Option<HirArm>> = arms.into_iter().map(Some).collect();
        let mut out = Vec::with_capacity(groups.len() + 1);
        for ((variant, idx), field_ty) in groups.iter().zip(plan) {
            let Some((field_ty, fallthrough)) = field_ty else {
                out.push(slots[idx[0]].take().expect("each arm is used once"));
                continue;
            };
            let mut inner = Vec::with_capacity(idx.len());
            let mut outer = None;
            for &i in idx {
                let arm = slots[i].take().expect("each arm is used once");
                let HirPat::Variant {
                    enum_name,
                    tag,
                    fields: HirPatFields::Tuple(mut parts),
                    ..
                } = arm.pat
                else {
                    unreachable!("split arms carry one payload pattern")
                };
                outer.get_or_insert((enum_name, tag));
                inner.push(HirArm {
                    pat: parts.pop().expect("one payload pattern"),
                    body: arm.body,
                });
            }
            if fallthrough {
                let shared = catch_all.as_ref().expect("checked shareable catch-all");
                inner.push(HirArm {
                    pat: HirPat::Wild,
                    body: shared.body,
                });
            }
            let (enum_name, tag) = outer.expect("a group has an arm");
            let t = b.temp("nest", Some(field_ty.clone()));
            let scrutinee = self.synth(b, span, HirKind::Local(t), Some(field_ty.clone()));
            let inner = self.split_nested(b, span, Some(&field_ty), ty.clone(), inner);
            let body = self.synth(b, span, HirKind::Match { scrutinee, arms: inner }, ty.clone());
            out.push(HirArm {
                pat: HirPat::Variant {
                    enum_name,
                    variant: variant.clone(),
                    tag,
                    fields: HirPatFields::Tuple(vec![HirPat::Bind(t)]),
                },
                body,
            });
        }
        out.extend(catch_all);
        out
    }

    /// Whether `pats`, tried in order on a value of type `ty`, match every
    /// value: a catch-all, or each variant covered (recursively for its
    /// one payload).
    fn exhaustive(&self, pats: &[&HirPat], ty: &Ty) -> bool {
        if pats.iter().any(|p| irrefutable(p)) {
            return true;
        }
        let Some(HirPat::Variant { enum_name, .. }) = pats.first() else {
            return false;
        };
        let t = strip_readonly(ty);
        let variants: Vec<String> = if is_option_ty(t) {
            vec!["Some".into(), "None".into()]
        } else if result_ok_err(t).is_some() {
            vec!["Ok".into(), "Err".into()]
        } else {
            match self.checker.enum_variants(enum_name) {
                Some(vars) => vars.into_iter().map(|(n, _, _)| n).collect(),
                None => return false,
            }
        };
        variants.iter().all(|v| {
            let mut subs = Vec::new();
            for p in pats {
                let HirPat::Variant { variant, fields, .. } = p else {
                    return false;
                };
                if variant != v {
                    continue;
                }
                match fields {
                    HirPatFields::Unit => return true,
                    HirPatFields::Tuple(parts) if parts.iter().all(irrefutable) => return true,
                    HirPatFields::Tuple(parts) if parts.len() == 1 => subs.push(&parts[0]),
                    _ => return false,
                }
            }
            !subs.is_empty()
                && self
                    .variant_field_tys(enum_name, v, Some(ty))
                    .first()
                    .is_some_and(|field| self.exhaustive(&subs, field))
        })
    }

    fn variant_pat(&self, enum_name: &str, variant: &str, fields: HirPatFields) -> HirPat {
        HirPat::Variant {
            enum_name: enum_name.to_string(),
            variant: variant.to_string(),
            tag: self.checker.tag_for(enum_name, variant),
            fields,
        }
    }

    fn pattern(&mut self, b: &mut BodyBuilder, pat: &Pattern<'_>, scrut_ty: Option<&Ty>) -> HirPat {
        match pat {
            Pattern::Wildcard | Pattern::Default => HirPat::Wild,
            Pattern::Binding { name } => HirPat::Bind(b.local(name, scrut_ty.cloned(), LocalKind::Pattern)),
            Pattern::Integer(n) => HirPat::Int(*n),
            Pattern::Constructor { enum_name, variant_name, payload } => {
                let field_tys = self.variant_field_tys(enum_name, variant_name, scrut_ty);
                let fields = match payload {
                    PatternPayload::Unit => HirPatFields::Unit,
                    PatternPayload::Tuple(items) => HirPatFields::Tuple(
                        items
                            .iter()
                            .enumerate()
                            .map(|(i, (_, p))| {
                                let ty = field_tys.get(i).cloned();
                                self.pattern(b, p, ty.as_ref())
                            })
                            .collect(),
                    ),
                    // Named fields become positional ones in declaration
                    // order (`_` for a field the pattern leaves out), so a
                    // record arm binds its payload like a tuple arm.
                    PatternPayload::Record(fields) => {
                        let decl = self.checker.payload_tys_for(enum_name, variant_name);
                        let known = !decl.is_empty()
                            && fields.iter().all(|f| decl.iter().any(|(n, _)| n == f.name));
                        if known {
                            HirPatFields::Tuple(
                                decl.iter()
                                    .enumerate()
                                    .map(|(i, (name, _))| match fields.iter().find(|f| f.name == name.as_str()) {
                                        Some(f) => {
                                            let ty = field_tys.get(i).cloned();
                                            self.pattern(b, &f.pattern.1, ty.as_ref())
                                        }
                                        None => HirPat::Wild,
                                    })
                                    .collect(),
                            )
                        } else {
                            HirPatFields::Record(
                                fields
                                    .iter()
                                    .map(|f| (f.name.to_string(), self.pattern(b, &f.pattern.1, None)))
                                    .collect(),
                            )
                        }
                    }
                };
                self.variant_pat(enum_name, variant_name, fields)
            }
        }
    }

    /// Payload types of `enum_name::variant` at the scrutinee's instance.
    fn variant_field_tys(&self, enum_name: &str, variant: &str, scrut_ty: Option<&Ty>) -> Vec<Ty> {
        let scrut = scrut_ty.map(strip_readonly);
        if let Some(t) = scrut {
            if is_option_ty(t) && variant == "Some" {
                return option_inner(t).into_iter().collect();
            }
            if let Some((ok, err)) = result_ok_err(t) {
                return match variant {
                    "Ok" => vec![ok],
                    "Err" => vec![err],
                    _ => Vec::new(),
                };
            }
        }
        // A ground instance of a generic user enum binds its parameters.
        if let Some(Ty::App(_, args)) = scrut
            && let Some(payload) = super::lower::generic_enum_payload(self.checker, enum_name, variant, args)
        {
            return payload.into_iter().filter(layout::ty_is_closed).collect();
        }
        self.checker
            .enum_variants(enum_name)
            .and_then(|vars| vars.into_iter().find(|(n, _, _)| n == variant))
            .map(|(_, _, payload)| payload)
            .filter(|payload| payload.iter().all(layout::ty_is_closed))
            .unwrap_or_default()
    }

    fn let_pattern(&mut self, b: &mut BodyBuilder, pat: &LetPattern<'_>, ty: Option<&Ty>) -> HirPat {
        let ty = ty.map(strip_readonly);
        match pat {
            LetPattern::Wildcard => HirPat::Wild,
            LetPattern::Binding { name } => HirPat::Bind(b.local(name, ty.cloned(), LocalKind::Pattern)),
            LetPattern::Tuple(items) => {
                let tys: Vec<Ty> = match ty {
                    Some(Ty::Tuple(tys)) => tys.clone(),
                    _ => Vec::new(),
                };
                HirPat::Tuple(
                    items
                        .iter()
                        .enumerate()
                        .map(|(i, p)| self.let_pattern(b, p, tys.get(i)))
                        .collect(),
                )
            }
            LetPattern::Record(fields) => {
                let tys: Vec<(String, Ty)> = match ty {
                    Some(Ty::Record { fields }) => fields.clone(),
                    _ => Vec::new(),
                };
                HirPat::Record(
                    fields
                        .iter()
                        .map(|f| {
                            let fty = tys.iter().find(|(n, _)| n == f.name).map(|(_, t)| t.clone());
                            (f.name.to_string(), self.let_pattern(b, &f.pattern, fty.as_ref()))
                        })
                        .collect(),
                )
            }
        }
    }

    fn lambda(&mut self, b: &mut BodyBuilder, node: &Output<'_>, args: &Output<'_>, body: &Output<'_>) -> HirId {
        let name = format!("{}::<lambda@{}>", b.body.name, node.0.start);
        let mut inner = BodyBuilder::new(&name, BodyKind::Lambda, span_of(node));
        inner.outer = b.outer.clone();
        inner.outer.extend(b.scopes.iter().cloned());
        let fn_ty = self.ty_of(node);
        if let Some(Ty::Fun(_, ret)) = fn_ty.as_ref().map(strip_readonly) {
            let ret = final_ret(ret);
            inner.body.ret_layout = layout::of_resolved(self.checker, &ret);
            inner.body.ret = Some(ret);
        }
        self.params(&mut inner, args, None);
        let root = self.expr(&mut inner, body);
        inner.body.root = Some(root);
        // Captures resolved through `outer` refer to `b`'s locals only when
        // they came from `b`'s own scopes; deeper ones are re-captured by `b`.
        let captured: Vec<String> = inner
            .body
            .captures
            .iter()
            .map(|(_, inner_id)| inner.body.locals[inner_id.0 as usize].name.clone())
            .collect();
        for (i, name) in captured.iter().enumerate() {
            if let Some(outer) = b.lookup(name) {
                inner.body.captures[i].0 = outer;
                b.body.locals[outer.0 as usize].captured = true;
                // A capture `b` only relays to this lambda has no read of
                // its own to type it.
                let inner_local = inner.body.captures[i].1;
                if b.body.locals[outer.0 as usize].ty.is_none() {
                    b.body.locals[outer.0 as usize].ty = inner.body.locals[inner_local.0 as usize].ty.clone();
                }
            }
        }
        let index = self.module.bodies.len();
        self.module.bodies.push(inner.finish());
        self.emit(b, node, HirKind::Lambda { body: index })
    }
}

/// `List<T>` → `List`, for matching an instance head to its source text.
fn head_str(text: &str) -> &str {
    text.split('<').next().unwrap_or(text).trim()
}

fn head_name(ty: &Ty) -> String {
    match strip_readonly(ty) {
        Ty::Con(n) => n.rsplit("::").next().unwrap_or(n).to_string(),
        Ty::App(head, _) => head_name(head),
        other => other.to_string(),
    }
}

/// The `Function` inside a `Method` wrapper.
fn fn_node<'a, 'e>(node: &'a Output<'e>) -> &'a Output<'e> {
    match node.1.as_ref() {
        Expression::Method(_, inner) => fn_node(inner),
        _ => node,
    }
}

fn fn_name<'e>(node: &Output<'e>) -> Option<&'e str> {
    match fn_node(node).1.as_ref() {
        Expression::Function { name, body: Some(_), .. } => Some(name),
        _ => None,
    }
}

/// The result of a curried `A -> B -> R` function type.
fn final_ret(ty: &Ty) -> Ty {
    match strip_readonly(ty) {
        Ty::Fun(_, ret) => final_ret(ret),
        other => other.clone(),
    }
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}::{name}")
    }
}

fn index_kind(ty: Option<&Ty>) -> IndexKind {
    // `Matrix<D>` indexes its data.
    if let Some(data) = ty.map(strip_readonly).and_then(crate::typechecking::aggregate_arith::unwrap_matrix_ty) {
        return index_kind(Some(data));
    }
    match ty.map(strip_readonly) {
        Some(Ty::Con(n)) if n == coil_ty::STRING => IndexKind::String,
        Some(Ty::List(_) | Ty::Array { .. }) => IndexKind::Array,
        Some(Ty::App(head, _)) if matches!(head.as_ref(), Ty::Con(n) if n == common::BUILTIN_VEC_TYPE) => {
            IndexKind::Array
        }
        Some(Ty::Record { .. }) => IndexKind::Dict,
        Some(Ty::App(head, _)) if matches!(head.as_ref(), Ty::Con(n) if n == "Dict") => IndexKind::Dict,
        Some(Ty::Tuple(_)) => IndexKind::Tuple,
        _ => IndexKind::Other,
    }
}

/// The primitive lane of `l op r`, or [`BinOp::Overloaded`].
pub(crate) fn resolve_bin(op: &'static str, l: Option<&Ty>, r: Option<&Ty>) -> BinOp {
    #[derive(PartialEq)]
    enum Lane {
        Int,
        Float,
        Str,
        Bool,
        Other,
    }
    let lane = |ty: Option<&Ty>| match ty.map(strip_readonly) {
        Some(Ty::Con(n)) if n == coil_ty::INT || n == coil_ty::BYTE => Lane::Int,
        Some(Ty::Con(n)) if n == coil_ty::FLOAT => Lane::Float,
        Some(Ty::Con(n)) if n == coil_ty::STRING => Lane::Str,
        Some(Ty::Con(n)) if n == coil_ty::BOOL => Lane::Bool,
        _ => Lane::Other,
    };
    let (l, r) = (lane(l), lane(r));
    let same = |want: Lane| l == want && r == want;
    let cmp = |op: &str| match op {
        "==" => Some(BinOp::Eq),
        "!=" => Some(BinOp::Ne),
        "<" => Some(BinOp::Lt),
        "<=" => Some(BinOp::Le),
        ">" => Some(BinOp::Gt),
        ">=" => Some(BinOp::Ge),
        _ => None,
    };
    if let Some(c) = cmp(op) {
        let primitive = l == r && l != Lane::Other;
        return if primitive { c } else { BinOp::Overloaded(op) };
    }
    if same(Lane::Int) {
        return match op {
            "+" => BinOp::IntAdd,
            "-" => BinOp::IntSub,
            "*" => BinOp::IntMul,
            "/" => BinOp::IntDiv,
            "%" => BinOp::IntRem,
            "**" => BinOp::IntPow,
            "<<" => BinOp::Shl,
            ">>" => BinOp::Shr,
            "&" => BinOp::BitAnd,
            "|" => BinOp::BitOr,
            "^" => BinOp::BitXor,
            _ => BinOp::Overloaded(op),
        };
    }
    if same(Lane::Float) {
        return match op {
            "+" => BinOp::FloatAdd,
            "-" => BinOp::FloatSub,
            "*" => BinOp::FloatMul,
            "/" => BinOp::FloatDiv,
            "%" => BinOp::FloatRem,
            "**" => BinOp::FloatPow,
            _ => BinOp::Overloaded(op),
        };
    }
    if op == "+" && same(Lane::Str) {
        return BinOp::StrConcat;
    }
    if same(Lane::Bool) {
        return match op {
            "&" => BinOp::BitAnd,
            "|" => BinOp::BitOr,
            "^" => BinOp::BitXor,
            _ => BinOp::Overloaded(op),
        };
    }
    BinOp::Overloaded(op)
}

/// A pattern that matches every value.
fn irrefutable(pat: &HirPat) -> bool {
    matches!(pat, HirPat::Wild | HirPat::Bind(_))
}

/// The one payload pattern of a single-field variant pattern.
fn nested_payload(pat: &HirPat) -> Option<&HirPat> {
    match pat {
        HirPat::Variant {
            fields: HirPatFields::Tuple(parts),
            ..
        } if parts.len() == 1 => parts.first(),
        _ => None,
    }
}

/// A catch-all whose body may also run as an inner match's last arm: it
/// binds nothing the body reads, and the body plans nothing per node that
/// two emissions would clash on (lambdas, nested matches, lets, loops).
fn shareable_catch_all(body: &HirBody, arm: &HirArm) -> bool {
    let bound = match arm.pat {
        HirPat::Wild => None,
        HirPat::Bind(l) => Some(l),
        _ => return false,
    };
    let mut ok = true;
    super::lower::visit(body, arm.body, &mut |e| {
        ok &= !matches!(
            &e.kind,
            HirKind::Lambda { .. }
                | HirKind::Match { .. }
                | HirKind::Let { .. }
                | HirKind::LetPat { .. }
                | HirKind::Loop { .. }
                | HirKind::ForIn { .. }
                | HirKind::Defer { .. }
                | HirKind::Yield { .. }
        ) && !matches!(e.kind, HirKind::Local(l) if Some(l) == bound);
    });
    ok
}

fn restore(mut arms: Vec<HirArm>, catch_all: Option<HirArm>) -> Vec<HirArm> {
    arms.extend(catch_all);
    arms
}

#[cfg(test)]
#[path = "build.tests.rs"]
mod tests;


/// The type a `?` miss returns: the function's own return type, or a
/// coroutine's yielded type (its final value).
fn miss_ret(b: &BodyBuilder) -> Option<Ty> {
    let ret = b.body.ret.clone();
    if !b.body.is_coro {
        return ret;
    }
    match ret.as_ref().map(strip_readonly) {
        Some(Ty::App(head, args)) if matches!(head.as_ref(), Ty::Con(n) if n == "coroutine") && args.len() == 2 => {
            Some(args[0].clone())
        }
        _ => ret,
    }
}
