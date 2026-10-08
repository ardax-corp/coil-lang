//! Attribute expansion (`#[derive(...)]`, user `attr`, etc.).
//!
//! Runs before the ID pre-walk and typechecking. Every `#[derive(X)]` — the
//! built-ins included, which live in `compiler/src/prelude/derive.hy` — and
//! every attribute macro is recorded as a pending macro for the pipeline to
//! run ([`crate::macros`]); this pass adds the type-name `Show` / `String`
//! defaults and checks attribute placement. Compile-time FFI is
//! `extern "lib" { fn …; }` only — `#[ffi]` is rejected.

use parser::{
    SimpleSpan,
    ast::{AttrArgs, Attribute, EnumVariantPayload, Expression, Output, Visibility},
};
use reporting::{ErrorCode, Message};

use crate::macros::{CallPosition, MacroDecl, MacroKind, PendingMacro};

const KNOWN_ATTRS: &[&str] = &["derive", "ffi", "test", "max_depth", "repr"];

/// Result of attribute expansion before typechecking.
#[derive(Default, Clone)]
pub struct ExpandResult {
    pub messages: Vec<Message>,
    /// `derive` / macro `attr` items this file declares (lowered to functions).
    pub macro_decls: Vec<MacroDecl>,
    /// Uses of derives / attributes that are not built in: user macros the
    /// pipeline resolves through `use`, or errors if nothing provides them.
    pub pending: Vec<PendingMacro>,
}

/// Expand every supported attribute on a program AST.
#[cfg(test)]
pub fn expand_program(ast: &mut Output<'_>) -> ExpandResult {
    expand_program_in(ast, "")
}

/// [`expand_program`] for a file of module `module` (lowers macro items first).
pub fn expand_program_in(ast: &mut Output<'_>, module: &str) -> ExpandResult {
    let lowered = crate::macros::lower::lower_program(ast, module);
    let Expression::Program(children) = ast.1.as_mut() else {
        return ExpandResult::default();
    };
    let mut messages = lowered.messages;
    let mut pending = Vec::new();
    messages.extend(expand_decls(children, &mut pending));
    record_macro_calls(children, &mut pending);
    ExpandResult {
        messages,
        macro_decls: lowered.decls,
        pending,
    }
}

/// [`expand_program_in`] for a file as written (not macro output): also
/// rejects module-qualified `impl` heads, which only macros write.
pub fn expand_source_in(ast: &mut Output<'_>, module: &str) -> ExpandResult {
    let qualified = reject_qualified_impl_heads(ast);
    let mut expand = expand_program_in(ast, module);
    expand.messages.extend(qualified);
    expand
}

/// `impl m::Trait for T` is how generated code names a provider's trait
/// without a `use`; source written by hand imports the trait instead.
fn reject_qualified_impl_heads(ast: &Output<'_>) -> Vec<Message> {
    let Expression::Program(children) = ast.1.as_ref() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for node in children {
        if let Expression::TypeClassImpl { class, .. } = node.1.as_ref()
            && let Some((module, name)) = class.rsplit_once("::")
        {
            let mut msg = Message::error(
                ErrorCode::GenericTypeError,
                format!("module-qualified trait `{class}` in an `impl` head is only written by macros"),
                node.0.into_range(),
            );
            msg.with_help(format!("`use {module}::{name};` and write `impl {name} for …`"));
            out.push(msg);
        }
    }
    out
}

/// Record every `name!(…)` call. Only the outermost call of a nest is
/// recorded: calls in its arguments are part of its input, and expand in its
/// output next round.
fn record_macro_calls(children: &[Output<'_>], pending: &mut Vec<PendingMacro>) {
    for child in children {
        match statement_call(child) {
            Some(call) => push_call(call, CallPosition::Item, pending),
            None => record_in(child, pending),
        }
    }
}

fn record_in(node: &Output<'_>, pending: &mut Vec<PendingMacro>) {
    match node.1.as_ref() {
        Expression::MacroCall { .. } => push_call(node, CallPosition::Expr, pending),
        Expression::Block(items) => {
            for item in items {
                match statement_call(item) {
                    Some(call) => push_call(call, CallPosition::Stmt, pending),
                    None => record_in(item, pending),
                }
            }
        }
        _ => node.1.for_each_child(&mut |c| record_in(c, pending)),
    }
}

/// The call of a `name!(…);` statement.
pub(crate) fn statement_call<'b, 'a>(node: &'b Output<'a>) -> Option<&'b Output<'a>> {
    let inner = match node.1.as_ref() {
        Expression::Statement(s) => s,
        _ => node,
    };
    let Expression::ExprStatement(e) = inner.1.as_ref() else {
        return None;
    };
    let mut e = e;
    while let Expression::Expr(inner) = e.1.as_ref() {
        e = inner;
    }
    matches!(e.1.as_ref(), Expression::MacroCall { .. }).then_some(e)
}

fn push_call(call: &Output<'_>, position: CallPosition, pending: &mut Vec<PendingMacro>) {
    let Expression::MacroCall { name, .. } = call.1.as_ref() else {
        return;
    };
    pending.push(PendingMacro {
        kind: MacroKind::Function,
        name: name.to_string(),
        target: call.0,
        position,
        owner: None,
        args: Vec::new(),
        range: call.0.into_range(),
        member_attrs: Vec::new(),
        from_provider: None,
    });
}

/// Diagnostic for a pending macro nothing in scope provides.
pub fn unresolved_macro_message(p: &PendingMacro) -> Message {
    match p.kind {
        MacroKind::Derive => {
            let mut msg = Message::error(
                ErrorCode::GenericTypeError,
                format!("Cannot derive unknown or non-derivable trait `{}`", p.name),
                p.range.clone(),
            );
            msg.with_help(format!(
                "built-in derives are: {}; or import a `derive {}` with `use`",
                crate::macros::PRELUDE_DERIVES.join(", "),
                p.name
            ));
            msg
        }
        MacroKind::Attr => Message::error(
            ErrorCode::GenericTypeError,
            format!("Unknown attribute `{}`", p.name),
            p.range.clone(),
        ),
        MacroKind::Function => {
            let mut msg = Message::error(
                ErrorCode::GenericTypeError,
                format!("unknown macro `{}!`", p.name),
                p.range.clone(),
            );
            msg.with_help(format!(
                "import a `macro {}` with `use module::{{{}}};`",
                p.name, p.name
            ));
            msg
        }
    }
}

fn pending_attr(name: &str, args: &AttrArgs<'_>, target: SimpleSpan, owner: Option<&str>) -> PendingMacro {
    PendingMacro {
        kind: MacroKind::Attr,
        name: name.to_string(),
        target,
        position: CallPosition::Decl,
        owner: owner.map(str::to_string),
        args: crate::macros::encode::attr_args(args),
        range: target.into_range(),
        member_attrs: Vec::new(),
        from_provider: None,
    }
}

/// Field / variant attribute names on a class or enum (owned by derives).
fn member_attr_names(members: &[Output<'_>]) -> Vec<String> {
    let mut out = Vec::new();
    for m in members {
        let attrs = match m.1.as_ref() {
            Expression::Field { attrs, .. } | Expression::EnumVariant { attrs, .. } => attrs,
            _ => continue,
        };
        for a in attrs {
            if !out.iter().any(|n: &String| n == a.name) {
                out.push(a.name.to_string());
            }
        }
    }
    out
}

fn derive_traits_from_attrs<'a>(attrs: &[Attribute<'a>]) -> Vec<&'a str> {
    let mut out = Vec::new();
    for attr in attrs {
        if attr.name == "derive"
            && let AttrArgs::Idents(idents) = &attr.args {
                out.extend(idents.iter().copied());
            }
    }
    out
}

fn strip_processed_attrs(attrs: &mut Vec<Attribute<'_>>) {
    attrs.retain(|a| a.name != "derive" && a.name != "ffi");
}

/// Check built-in attribute placement. Returns the attributes that are
/// neither built in nor legacy `attr` decorators: user attribute macros,
/// resolved (or reported) by the pipeline.
fn validate_attrs<'x, 'a>(
    attrs: &'x [Attribute<'a>],
    target: &str,
    messages: &mut Vec<Message>,
    span: SimpleSpan,
    is_ffi: bool,
) -> Vec<&'x Attribute<'a>> {
    let mut unknown = Vec::new();
    for attr in attrs {
        if attr.name == "test" {
            messages.push(Message::error(
                ErrorCode::GenericTypeError,
                "`#[test]` is not supported; use `test(\"desc\") { … }`".to_string(),
                span.into_range(),
            ));
        }
        if attr.name == "max_depth" && target != "function" {
            messages.push(Message::error(
                ErrorCode::GenericTypeError,
                format!("Attribute `max_depth` is not valid on {}", target),
                span.into_range(),
            ));
        }
        if attr.name == "repr" && target != "enum" {
            messages.push(Message::error(
                ErrorCode::GenericTypeError,
                format!("Attribute `repr` is not valid on {}", target),
                span.into_range(),
            ));
        }
        if !KNOWN_ATTRS.contains(&attr.name) {
            if is_ffi {
                messages.push(Message::error(
                    ErrorCode::GenericTypeError,
                    format!(
                        "Attribute macro `{}` cannot be applied to FFI functions",
                        attr.name
                    ),
                    span.into_range(),
                ));
            } else {
                unknown.push(attr);
            }
        }
    }
    unknown
}

fn unwrap_disc_expr<'e, 'a>(expr: &'a Expression<'e>) -> &'a Expression<'e> {
    match expr {
        Expression::Expr(e) | Expression::Group(e) | Expression::Positive(e) => {
            unwrap_disc_expr(e.1.as_ref())
        }
        other => other,
    }
}

fn disc_lit_kind(expr: &Expression<'_>) -> Option<&'static str> {
    match unwrap_disc_expr(expr) {
        Expression::Integer(_) => Some("int"),
        Expression::Negate(inner) => match unwrap_disc_expr(inner.1.as_ref()) {
            Expression::Integer(_) => Some("int"),
            Expression::Float(_) => Some("float"),
            _ => None,
        },
        Expression::Float(_) => Some("float"),
        Expression::String(_) => Some("string"),
        Expression::Bool(_) => Some("bool"),
        _ => None,
    }
}

/// Backing type when every case is a unit `= lit` (inferred or `#[repr]`).
pub(crate) fn scalar_backing_ty_name<'a>(
    attrs: &[Attribute<'a>],
    variants: &[Output<'a>],
) -> Option<&'a str> {
    let mut from_repr = None;
    for attr in attrs {
        if attr.name != "repr" {
            continue;
        }
        if let AttrArgs::Idents(ids) = &attr.args
            && ids.len() == 1
        {
            match ids[0] {
                "int" | "float" | "string" | "bool" => from_repr = Some(ids[0]),
                _ => {}
            }
        }
    }
    if variants.is_empty() {
        return None;
    }
    let mut inferred: Option<&str> = None;
    for v in variants {
        let Expression::EnumVariant {
            payload,
            discriminant,
            ..
        } = v.1.as_ref()
        else {
            continue;
        };
        if !matches!(payload, EnumVariantPayload::Unit) {
            return None;
        }
        let disc = discriminant.as_ref()?;
        let kind = disc_lit_kind(disc.1.as_ref())?;
        match inferred {
            None => inferred = Some(kind),
            Some(k) if k != kind => return None,
            Some(_) => {}
        }
    }
    from_repr.or(inferred)
}

fn expand_decls<'a>(decls: &mut Vec<Output<'a>>, pending: &mut Vec<PendingMacro>) -> Vec<Message> {
    let mut messages = Vec::new();
    let mut i = 0;
    while i < decls.len() {
        let span = decls[i].0;

        // Compile-time FFI is `extern "lib" { fn …; }` only.
        // Attribute macros expand outermost first: only an item's first one
        // runs now; the rest stay on the item it is given and expand in the
        // next round (in the macro's output).
        if let Expression::Function { attrs, body, .. } = decls[i].1.as_mut() {
            let is_ffi_sig = body.is_none();
            if let Some(a) = validate_attrs(attrs, "function", &mut messages, span, is_ffi_sig).first() {
                pending.push(pending_attr(a.name, &a.args, span, None));
            }
            if attrs.iter().any(|a| a.name == "ffi") {
                messages.push(Message::error(
                    ErrorCode::GenericTypeError,
                    "`#[ffi]` is not supported; use `extern \"lib\" { fn …; }`".to_string(),
                    span.into_range(),
                ));
            } else if is_ffi_sig {
                messages.push(Message::error(
                    ErrorCode::GenericTypeError,
                    "Signature-only function requires `extern \"lib\" { fn …; }`".to_string(),
                    span.into_range(),
                ));
            }
        }

        // Attribute macros on impl methods.
        if let Expression::Implementation { methods, owner, .. } = decls[i].1.as_ref() {
            let owner: &str = owner;
            for method in methods {
                if let Expression::Method(_, func_out) = method.1.as_ref()
                    && let Expression::Function { attrs, .. } = func_out.1.as_ref()
                    && let Some(a) = validate_attrs(attrs, "function", &mut messages, span, false).first()
                {
                    pending.push(pending_attr(a.name, &a.args, method.0, Some(owner)));
                }
            }
        }

        /// A class or enum whose derives to record.
        struct Job<'a> {
            name: &'a str,
            generic: bool,
            derives: Vec<&'a str>,
            scalar_backing: Option<&'a str>,
        }
        let job = match decls[i].1.as_ref() {
            Expression::EnumDecl {
                docs: _,
                name,
                type_params,
                attrs,
                variants,
            } => {
                // An attribute macro runs before the type's derives, which stay
                // on the item it receives.
                if let Some(a) = validate_attrs(attrs, "enum", &mut messages, span, false).first() {
                    pending.push(pending_attr(a.name, &a.args, span, None));
                    None
                } else {
                Some(Job {
                    name,
                    generic: !type_params.is_empty(),
                    derives: derive_traits_from_attrs(attrs),
                    scalar_backing: scalar_backing_ty_name(attrs, variants),
                })
                }
            }
            Expression::Class {
                docs: _,
                name,
                type_params,
                attrs,
                ..
            } => {
                if let Some(a) = validate_attrs(attrs, "class", &mut messages, span, false).first() {
                    pending.push(pending_attr(a.name, &a.args, span, None));
                    None
                } else {
                Some(Job {
                    name,
                    generic: !type_params.is_empty(),
                    derives: derive_traits_from_attrs(attrs),
                    scalar_backing: None,
                })
                }
            }
            _ => None,
        };

        let synthesized = job.map(|job| {
            expand_derives(ExpandDerivesArgs {
                span,
                name: job.name,
                generic: job.generic,
                derives: &job.derives,
                scalar_backing: job.scalar_backing,
                decls,
                pending,
            })
        });

        // `#[helper(...)]` on fields / variants belongs to a user derive.
        if let Expression::EnumDecl { variants: members, .. }
        | Expression::Class { fields: members, .. } = decls[i].1.as_ref()
        {
            let member_attrs = member_attr_names(members);
            if !member_attrs.is_empty() {
                let mut owned = false;
                for p in pending
                    .iter_mut()
                    .filter(|p| p.kind == MacroKind::Derive && p.target == span)
                {
                    p.member_attrs = member_attrs.clone();
                    owned = true;
                }
                if !owned {
                    for name in &member_attrs {
                        let mut msg = Message::error(
                            ErrorCode::GenericTypeError,
                            format!("Unknown attribute `{name}`"),
                            span.into_range(),
                        );
                        msg.with_help(
                            "field and variant attributes belong to a derive macro (`derive D(TypeDecl t) -> Code attrs(name)`)"
                                .to_string(),
                        );
                        messages.push(msg);
                    }
                }
            }
        }

        if let Some(impls) = synthesized {
            if let Expression::EnumDecl { attrs, .. } | Expression::Class { attrs, .. } =
                decls[i].1.as_mut()
            {
                strip_processed_attrs(attrs);
            }
            let n = impls.len();
            for (offset, impl_node) in impls.into_iter().enumerate() {
                decls.insert(i + 1 + offset, impl_node);
            }
            i += 1 + n;
        } else {
            i += 1;
        }
    }
    messages
}

struct ExpandDerivesArgs<'args, 'a> {
    span: SimpleSpan,
    name: &'a str,
    generic: bool,
    derives: &'args [&'a str],
    scalar_backing: Option<&'a str>,
    decls: &'args [Output<'a>],
    pending: &'args mut Vec<PendingMacro>,
}

/// Record a class / enum's derives as derive macros to run (built-ins live in
/// `compiler/src/prelude/derive.hy`) and add the type-name `Show` / `String`
/// defaults the derives and explicit impls leave uncovered.
fn expand_derives<'a>(args: ExpandDerivesArgs<'_, 'a>) -> Vec<Output<'a>> {
    let ExpandDerivesArgs {
        span,
        name,
        generic,
        derives,
        scalar_backing,
        decls,
        pending,
    } = args;
    // A generic type's derives expand to bounded instances
    // (`impl Show for Box<T: Show>`, #552) via `TypeDecl::impl_head`.
    for &trait_name in derives {
        pending.push(pending_derive(trait_name, span));
    }
    // The type-name `Show` / `String` defaults are for non-generic types.
    if generic {
        return Vec::new();
    }
    let mut out = Vec::new();
    push_default_display_impls(span, name, derives, decls, scalar_backing, &mut out);
    out
}

/// Auto-generate FQN-only `Show`/`String` when neither derive nor an explicit
/// `impl` covers the type. Bodies return the type name as a string literal
/// (same display as `typeof self` for non-generic types).
    ///
/// Inserted beside the type so typecheck sees the instance before later
/// `fn main` / statements. Script-style top-level match/expr after the type
/// should use `fn main` — these impls bind function entries and would
/// otherwise steal `program_start_offset` (DCE then drops the match body).
fn push_default_display_impls<'a>(
    span: SimpleSpan,
    name: &'a str,
    derives: &[&str],
    decls: &[Output<'a>],
    scalar_backing: Option<&'a str>,
    out: &mut Vec<Output<'a>>,
) {
    if !derives.contains(&"Show") && !has_explicit_impl(decls, "Show", name) {
        if let Some(backing) = scalar_backing {
            out.push(synth_show_scalar_enum(span, name, backing));
        } else {
            out.push(synth_show_type_name(span, name));
        }
    }
    if !derives.contains(&"String") && !has_explicit_impl(decls, "String", name) {
        if let Some(backing) = scalar_backing {
            out.push(synth_string_scalar_enum(span, name, backing));
        } else {
            out.push(synth_string_type_name(span, name));
        }
    }
}

/// Last `::` segment of a path (`json::Serialize` -> `Serialize`).
fn path_leaf(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or(path)
}

/// A hand-written `impl class for ty_name` in `decls`. Module-qualified
/// heads (`impl json::Serialize for m::Point`) match on their last segment.
fn has_explicit_impl(decls: &[Output<'_>], class: &str, ty_name: &str) -> bool {
    decls.iter().any(|d| {
        matches!(
            d.1.as_ref(),
            Expression::TypeClassImpl {
                class: c,
                args,
                ..
            } if path_leaf(c) == path_leaf(class)
                && args.first().is_some_and(|a| match a.1.as_ref() {
                    Expression::Type(n) | Expression::Identifier(n) => *n == ty_name,
                    Expression::TypeProjection { name, .. } => *name == ty_name,
                    _ => false,
                })
        )
    })
}

fn synth_show_type_name<'a>(span: SimpleSpan, name: &'a str) -> Output<'a> {
    let p = leak(format!("__show_{}", name));
    let body = str_lit(span, name);
    let show_m = method_fn(
        span,
        "show",
        vec![arg(span, name, p)],
        "string",
        block_return(span, body),
    );
    typeclass_impl(span, "Show", name, vec![show_m])
}

fn synth_string_type_name<'a>(span: SimpleSpan, name: &'a str) -> Output<'a> {
    let p = leak(format!("__str_{}", name));
    let body = str_lit(span, name);
    let m = method_fn(
        span,
        "to_string",
        vec![arg(span, name, p)],
        "string",
        block_return(span, body),
    );
    typeclass_impl(span, "String", name, vec![m])
}

fn pending_derive(trait_name: &str, span: SimpleSpan) -> PendingMacro {
    PendingMacro {
        kind: MacroKind::Derive,
        name: trait_name.to_string(),
        target: span,
        position: CallPosition::Decl,
        owner: None,
        args: Vec::new(),
        range: span.into_range(),
        member_attrs: Vec::new(),
        from_provider: None,
    }
}


pub(crate) fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

/// Mint a unique span for each synthetic node.
    ///
/// Sharing the owning `enum`/`class` span across every derived expression
/// makes span-keyed codegen lookups (`lookup_for_codegen_span`, `%v` Show
/// lowering) collide and pick up the declaration's `unit` type. Unique
/// micro-spans keep the ID/infer caches aligned; expand diagnostics still
/// use the real header span from `expand_decls`.
pub(crate) fn fresh_span() -> SimpleSpan {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0x4000_0000);
    let start = NEXT.fetch_add(1, Ordering::Relaxed);
    SimpleSpan::from(start..start + 1)
}

fn at<'a>(_diag_span: SimpleSpan, expr: Expression<'a>) -> Output<'a> {
    (fresh_span(), Box::new(expr))
}

fn ty_name<'a>(span: SimpleSpan, name: &'a str) -> Output<'a> {
    at(span, Expression::Type(name))
}

/// Parse `int` / `string`, `Vec<elem>`, or legacy dynamic `[elem]` (maps to `Vec`).
fn ty_ret<'a>(span: SimpleSpan, ret: &'a str) -> Output<'a> {
    if let Some(elem) = ret
        .strip_prefix("Vec<")
        .and_then(|s| s.strip_suffix('>'))
    {
        return at(
            span,
            Expression::TypeApp {
                name: "Vec",
                args: vec![ty_name(span, leak(elem.to_string()))],
            },
        );
    }
    if ret.len() >= 2 && ret.starts_with('[') && ret.ends_with(']') && !ret.contains(';') {
        let elem = &ret[1..ret.len() - 1];
        return at(
            span,
            Expression::TypeApp {
                name: "Vec",
                args: vec![ty_name(span, elem)],
            },
        );
    }
    ty_name(span, ret)
}

fn ident<'a>(span: SimpleSpan, name: &'a str) -> Output<'a> {
    at(span, Expression::Identifier(name))
}

fn str_lit<'a>(span: SimpleSpan, s: &'a str) -> Output<'a> {
    at(span, Expression::String(s))
}

fn stmt<'a>(span: SimpleSpan, inner: Output<'a>) -> Output<'a> {
    at(span, Expression::Statement(inner))
}

fn block_return<'a>(span: SimpleSpan, value: Output<'a>) -> Output<'a> {
    at(
        span,
        Expression::Block(vec![stmt(span, at(span, Expression::Return(value)))]),
    )
}

fn method_fn<'a>(
    span: SimpleSpan,
    name: &'a str,
    args: Vec<Output<'a>>,
    ret: &'a str,
    body: Output<'a>,
) -> Output<'a> {
    let func = at(
        span,
        Expression::Function {
            docs: vec![],
            attrs: vec![],
            name,
            is_coro: false,
            is_static: false,
            type_params: vec![],
            args: at(span, Expression::Fragment(args)),
            returns: Some(ty_ret(span, ret)),
            where_constraints: vec![],
            effects: None,
            body: Some(body),
        },
    );
    at(span, Expression::Method(Visibility::Private, func))
}

fn arg<'a>(span: SimpleSpan, ty: &'a str, name: &'a str) -> Output<'a> {
    at(
        span,
        Expression::Argument {
            docs: Vec::new(),
            ty: Some(ty_ret(span, ty)),
            name,
            is_rest: false,
        },
    )
}

fn typeclass_impl<'a>(
    span: SimpleSpan,
    class: &'a str,
    self_ty: &'a str,
    methods: Vec<Output<'a>>,
) -> Output<'a> {
    at(
        span,
        Expression::TypeClassImpl {
            class,
            args: vec![ty_name(span, self_ty)],
            type_params: Vec::new(),
            methods,
        },
    )
}

fn let_bind<'a>(span: SimpleSpan, ty: &'a str, name: &'a str, init: Output<'a>) -> Output<'a> {
    at(
        span,
        Expression::Fragment(vec![
            at(span, Expression::Variable(name, Some(ty_name(span, ty)))),
            init,
        ]),
    )
}

fn block_lets_return<'a>(
    span: SimpleSpan,
    lets: Vec<Output<'a>>,
    value: Output<'a>,
) -> Output<'a> {
    let mut items: Vec<Output<'a>> = lets.into_iter().map(|l| stmt(span, l)).collect();
    items.push(stmt(span, at(span, Expression::Return(value))));
    at(span, Expression::Block(items))
}



fn ufcs_nullary<'a>(span: SimpleSpan, recv: Output<'a>, method: &'a str) -> Output<'a> {
    at(
        span,
        Expression::Call {
            name: at(span, Expression::Access(recv, method)),
            args: None,
        },
    )
}

fn synth_show_scalar_enum<'a>(
    span: SimpleSpan,
    enum_name: &'a str,
    backing: &'a str,
) -> Output<'a> {
    let p = leak(format!("__show_{enum_name}"));
    let n = leak(format!("__show_n_{enum_name}"));
    let body = block_lets_return(
        span,
        vec![let_bind(span, backing, n, ident(span, p))],
        ufcs_nullary(span, ident(span, n), "show"),
    );
    let m = method_fn(
        span,
        "show",
        vec![arg(span, enum_name, p)],
        "string",
        body,
    );
    typeclass_impl(span, "Show", enum_name, vec![m])
}

fn synth_string_scalar_enum<'a>(
    span: SimpleSpan,
    enum_name: &'a str,
    backing: &'a str,
) -> Output<'a> {
    let p = leak(format!("__str_{enum_name}"));
    let n = leak(format!("__str_n_{enum_name}"));
    let body = block_lets_return(
        span,
        vec![let_bind(span, backing, n, ident(span, p))],
        ufcs_nullary(span, ident(span, n), "show"),
    );
    let m = method_fn(
        span,
        "to_string",
        vec![arg(span, enum_name, p)],
        "string",
        body,
    );
    typeclass_impl(span, "String", enum_name, vec![m])
}





#[cfg(test)]
#[path = "attrs.tests.rs"]
mod tests;
