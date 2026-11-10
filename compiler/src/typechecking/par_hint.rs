//! F3 parallelization hints: would-be IPA call bags refused on shared escapes.
//!
//! Does **not** insert locks or emit workers. See
//! [`docs/internals/par-lock-hints.md`](../../../docs/internals/par-lock-hints.md).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::Range;

use common::EffectFlags;
use parser::ast::{EnumConstructPayload, Expression, Output};

use crate::hir::par::ParForkSite;

pub use crate::hir::par::par_hint::{EscapeKind, NamedEscape, ParEscapeHint};

/// Would-be IPA sites refused because a lockable shared escape is in the bag.
///
/// Uses the same fork-shape detector as F0–F2, but with **all** user `fn`s as
/// candidate arms. Pure functions that already IPA are skipped. Panic / yield
/// impurity without a lockable resource is skipped.
///
/// `effects` are the checker's effect bits for this file's functions, by
/// bind name ([`TypedSidecar::fn_effects`]). `sites` are the module's fork
/// sites with every one of those functions taken as an arm
/// ([`HirModule::par_fork_sites`] over [`hint_arms`]).
///
/// [`TypedSidecar::fn_effects`]: super::infer::TypedSidecar::fn_effects
/// [`HirModule::par_fork_sites`]: crate::hir::HirModule::par_fork_sites
pub fn analyze_par_escape_hints(
    ast: &Output<'_>,
    effects: &HashMap<String, EffectFlags>,
    sites: &HashMap<String, ParForkSite>,
) -> Vec<ParEscapeHint> {
    let user_fns = hint_arms(effects);
    if user_fns.is_empty() {
        return Vec::new();
    }
    let mut bodies = HashMap::new();
    collect_fn_bodies(ast, &mut bodies);

    let mut out = Vec::new();
    let mut hinted = HashSet::new();

    for (name, site) in sites {
        let flags = effects
            .get(name)
            .copied()
            .unwrap_or_else(EffectFlags::empty);
        if flags.is_pure() {
            continue;
        }
        let Some(body) = bodies.get(name.as_str()).copied() else {
            continue;
        };
        let resources = named_escapes_in_fn(name, &bodies, &user_fns);
        if resources.is_empty() {
            continue;
        }
        hinted.insert(name.clone());
        out.push(ParEscapeHint {
            fn_name: name.clone(),
            span: site_span(body),
            resources,
            covering_lock: false,
        });
        let _ = site;
    }

    // Call bags only inside `with_lock` callbacks are not named-fn fork sites.
    for (name, body) in &bodies {
        if hinted.contains(name) {
            continue;
        }
        let flags = effects
            .get(name)
            .copied()
            .unwrap_or_else(EffectFlags::empty);
        if flags.is_pure() {
            continue;
        }
        if let Some((span, lock)) = covering_lock_bag(body, &user_fns) {
            let mut resources = named_escapes_in_fn(name, &bodies, &user_fns);
            if !resources
                .iter()
                .any(|r| r.name == lock.name && r.kind == lock.kind)
            {
                resources.insert(0, lock);
            }
            hinted.insert(name.clone());
            out.push(ParEscapeHint {
                fn_name: name.clone(),
                span,
                resources,
                covering_lock: true,
            });
        }
    }

    // If a named-fn site also has a covering lock wrapping a bag, upgrade.
    for hint in &mut out {
        if hint.covering_lock {
            continue;
        }
        if let Some(body) = bodies.get(hint.fn_name.as_str())
            && let Some((_, resource)) = covering_lock_bag(body, &user_fns) {
                hint.covering_lock = true;
                if !hint
                    .resources
                    .iter()
                    .any(|r| r.name == resource.name && r.kind == resource.kind)
                {
                    hint.resources.insert(0, resource);
                }
            }
    }

    out.sort_by(|a, b| a.fn_name.cmp(&b.fn_name));
    out
}

/// The candidate arms of a would-be call bag: every function with effects
/// on record.
pub fn hint_arms(effects: &HashMap<String, EffectFlags>) -> HashSet<String> {
    effects.keys().cloned().collect()
}

fn site_span(body: &Output<'_>) -> Range<usize> {
    body.0.into_range()
}

fn collect_fn_bodies<'a>(ast: &'a Output<'a>, out: &mut HashMap<String, &'a Output<'a>>) {
    match ast.1.as_ref() {
        Expression::Program(items) | Expression::Block(items) | Expression::Fragment(items) => {
            for item in items {
                collect_fn_bodies(item, out);
            }
        }
        Expression::Module(_, body) => collect_fn_bodies(body, out),
        Expression::Statement(inner)
        | Expression::Expr(inner)
        | Expression::ExprStatement(inner)
        | Expression::Group(inner) => collect_fn_bodies(inner, out),
        Expression::Function {
            name,
            body: Some(body),
            ..
        } => {
            out.insert((*name).to_string(), body);
            collect_fn_bodies(body, out);
        }
        Expression::Implementation { methods, .. } => {
            for m in methods {
                collect_fn_bodies(m, out);
            }
        }
        Expression::Method(_, inner) | Expression::Member(inner) => collect_fn_bodies(inner, out),
        _ => {}
    }
}

fn named_escapes_in_fn(
    fn_name: &str,
    bodies: &HashMap<String, &Output<'_>>,
    user_fns: &HashSet<String>,
) -> Vec<NamedEscape> {
    let mut found = BTreeSet::new();
    let mut stack = vec![fn_name.to_string()];
    let mut seen = HashSet::new();
    while let Some(name) = stack.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let Some(body) = bodies.get(&name) else {
            continue;
        };
        let mut aliases = HashMap::new();
        walk_escapes(body, user_fns, &mut found, &mut stack, &mut aliases);
    }
    found.into_iter().collect()
}

fn walk_escapes(
    ast: &Output<'_>,
    user_fns: &HashSet<String>,
    out: &mut BTreeSet<NamedEscape>,
    callees: &mut Vec<String>,
    aliases: &mut HashMap<String, EscapeKind>,
) {
    match ast.1.as_ref() {
        Expression::Program(items)
        | Expression::Block(items)
        | Expression::Fragment(items)
        | Expression::Array(items)
        | Expression::Tuple(items)
        | Expression::If(items) => {
            for item in items {
                walk_escapes(item, user_fns, out, callees, aliases);
                if let Some((name, kind)) = let_escape_alias(item, aliases) {
                    aliases.insert(name.to_string(), kind);
                    out.insert(NamedEscape {
                        kind,
                        name: name.to_string(),
                    });
                }
            }
        }
        Expression::Statement(inner)
        | Expression::Expr(inner)
        | Expression::ExprStatement(inner)
        | Expression::Group(inner)
        | Expression::Return(inner)
        | Expression::ImplicitReturn(inner)
        | Expression::Raise(inner)
        | Expression::Try(inner)
        | Expression::Negate(inner)
        | Expression::Not(inner)
        | Expression::LogicalNot(inner)
        | Expression::Positive(inner)
        | Expression::Cast(inner, _)
        | Expression::TypeOf(inner)
        | Expression::Readonly(inner)
        | Expression::OptionalAccess(inner, _)
        | Expression::Method(_, inner)
        | Expression::Member(inner) => walk_escapes(inner, user_fns, out, callees, aliases),
        Expression::Add(a, b)
        | Expression::Sub(a, b)
        | Expression::Mul(a, b)
        | Expression::Div(a, b)
        | Expression::Mod(a, b)
        | Expression::Pow(a, b)
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
        | Expression::Coalesce(a, b)
        | Expression::Assignment(a, b)
        | Expression::CompoundAssign(a, _, b) => {
            note_heap_mut_lhs(a, out);
            walk_escapes(a, user_fns, out, callees, aliases);
            walk_escapes(b, user_fns, out, callees, aliases);
        }
        Expression::Adjust { target, .. } => {
            note_heap_mut_lhs(target, out);
            walk_escapes(target, user_fns, out, callees, aliases);
        }
        Expression::Call { name, args } => {
            note_call_escape(name, args.as_deref(), user_fns, out, callees, aliases);
            walk_escapes(name, user_fns, out, callees, aliases);
            for a in args.iter().flatten() {
                walk_escapes(a, user_fns, out, callees, aliases);
            }
        }
        Expression::Dload(path) => {
            walk_escapes(path, user_fns, out, callees, aliases);
            note_ffi_names("dload", None, out);
        }
        Expression::Declare(args) => {
            note_ffi_names("declare", Some(args), out);
            for a in args {
                walk_escapes(a, user_fns, out, callees, aliases);
            }
        }
        Expression::Invoke(args) => {
            note_ffi_names("invoke", Some(args), out);
            for a in args {
                walk_escapes(a, user_fns, out, callees, aliases);
            }
        }
        Expression::Defer { body, .. } => walk_escapes(body, user_fns, out, callees, aliases),
        Expression::Dict(fields) => {
            for f in fields {
                walk_escapes(&f.value, user_fns, out, callees, aliases);
            }
        }
        Expression::Range { start, end, .. } => {
            walk_escapes(start, user_fns, out, callees, aliases);
            walk_escapes(end, user_fns, out, callees, aliases);
        }
        Expression::Shl(a, b) | Expression::Shr(a, b) => {
            walk_escapes(a, user_fns, out, callees, aliases);
            walk_escapes(b, user_fns, out, callees, aliases);
        }
        Expression::Construct { fields, .. } => match fields {
            EnumConstructPayload::Tuple(items) => {
                for item in items {
                    walk_escapes(item, user_fns, out, callees, aliases);
                }
            }
            EnumConstructPayload::Record(fields) => {
                for f in fields {
                    walk_escapes(&f.value, user_fns, out, callees, aliases);
                }
            }
            EnumConstructPayload::Unit => {}
        },
        Expression::Branch(cond, body) => {
            if let Some(c) = cond {
                walk_escapes(c, user_fns, out, callees, aliases);
            }
            walk_escapes(body, user_fns, out, callees, aliases);
        }
        Expression::Match { scrutinee, arms } => {
            walk_escapes(scrutinee, user_fns, out, callees, aliases);
            for arm in arms {
                walk_escapes(&arm.body, user_fns, out, callees, aliases);
            }
        }
        Expression::Loop {
            contracts: _,
            identifier,
            pattern: _,
            iterable,
            body,
        } => {
            if let Some(id) = identifier {
                walk_escapes(id, user_fns, out, callees, aliases);
            }
            walk_escapes(iterable, user_fns, out, callees, aliases);
            walk_escapes(body, user_fns, out, callees, aliases);
        }
        Expression::Variable(_, Some(init)) | Expression::Constant(_, Some(init), ..) => {
            walk_escapes(init, user_fns, out, callees, aliases)
        }
        Expression::LetDestructure { rhs, .. } => {
            walk_escapes(rhs, user_fns, out, callees, aliases)
        }
        Expression::Lambda { body, .. } => walk_escapes(body, user_fns, out, callees, aliases),
        Expression::Function {
            body: Some(body), ..
        } => walk_escapes(body, user_fns, out, callees, aliases),
        Expression::Index(base, Some(idx)) => {
            walk_escapes(base, user_fns, out, callees, aliases);
            walk_escapes(idx, user_fns, out, callees, aliases);
        }
        Expression::Index(base, None) | Expression::Access(base, _) => {
            walk_escapes(base, user_fns, out, callees, aliases)
        }
        Expression::NamedArg(_, v) => walk_escapes(v, user_fns, out, callees, aliases),
        _ => {}
    }
}

fn note_heap_mut_lhs(lhs: &Output<'_>, out: &mut BTreeSet<NamedEscape>) {
    let lhs = peel(lhs);
    match lhs.1.as_ref() {
        Expression::Index(base, _) | Expression::Access(base, _) => {
            if let Some(name) = ident_name(base) {
                out.insert(NamedEscape {
                    kind: EscapeKind::HeapObject,
                    name: name.to_string(),
                });
            }
        }
        _ => {}
    }
}

fn note_call_escape(
    name: &Output<'_>,
    args: Option<&[Output<'_>]>,
    user_fns: &HashSet<String>,
    out: &mut BTreeSet<NamedEscape>,
    callees: &mut Vec<String>,
    aliases: &HashMap<String, EscapeKind>,
) {
    let Some(callee) = callee_name(name) else {
        return;
    };
    let short = callee.rsplit("::").next().unwrap_or(callee);
    if user_fns.contains(callee) || user_fns.contains(short) {
        callees.push(if user_fns.contains(callee) {
            callee.to_string()
        } else {
            short.to_string()
        });
        return;
    }
    if is_format_like(short) {
        return;
    }
    if is_fd_endpoint(short) {
        let resource = fd_arg_name(args, aliases).unwrap_or_else(|| short.to_string());
        out.insert(NamedEscape {
            kind: EscapeKind::Fd,
            name: resource,
        });
        return;
    }
    if is_ffi_host(short) {
        note_ffi_names(short, args, out);
        return;
    }
    if is_mutex_host(short) {
        if matches!(short, "mutex" | "rwlock") {
            return;
        }
        let resource = first_ident_arg(args).unwrap_or(short);
        out.insert(NamedEscape {
            kind: EscapeKind::Mutex,
            name: resource.to_string(),
        });
    }
}

fn let_escape_alias<'a>(
    item: &'a Output<'a>,
    aliases: &HashMap<String, EscapeKind>,
) -> Option<(&'a str, EscapeKind)> {
    let Expression::Variable(name, Some(init)) = peel(item).1.as_ref() else {
        return None;
    };
    init_escape_kind(init, aliases).map(|kind| (*name, kind))
}

fn init_escape_kind(
    init: &Output<'_>,
    aliases: &HashMap<String, EscapeKind>,
) -> Option<EscapeKind> {
    let init = peel(init);
    let init = match init.1.as_ref() {
        Expression::Try(inner) => peel(inner),
        _ => init,
    };
    if let Expression::Identifier(n) = init.1.as_ref() {
        return aliases.get(*n).copied();
    }
    if matches!(
        init.1.as_ref(),
        Expression::Dload(_) | Expression::Declare(_) | Expression::Invoke(_)
    ) {
        return Some(EscapeKind::Ffi);
    }
    let Expression::Call { name, .. } = init.1.as_ref() else {
        return None;
    };
    let callee = callee_name(name)?;
    let short = callee.rsplit("::").next().unwrap_or(callee);
    if is_format_like(short) {
        return None;
    }
    if is_fd_endpoint(short) {
        return Some(EscapeKind::Fd);
    }
    if is_ffi_host(short) {
        return Some(EscapeKind::Ffi);
    }
    None
}

fn note_ffi_names(short: &str, args: Option<&[Output<'_>]>, out: &mut BTreeSet<NamedEscape>) {
    out.insert(NamedEscape {
        kind: EscapeKind::Ffi,
        name: short.to_string(),
    });
    if let Some(name) = first_ident_arg(args)
        && name != short {
            out.insert(NamedEscape {
                kind: EscapeKind::Ffi,
                name: name.to_string(),
            });
        }
}

fn is_format_like(short: &str) -> bool {
    matches!(short, "format" | "to_bytes" | "from_bytes")
}

fn fd_factory_name(short: &str) -> Option<&'static str> {
    match short {
        "stdout" => Some("stdout"),
        "stderr" => Some("stderr"),
        "stdin" => Some("stdin"),
        _ => None,
    }
}

fn is_ffi_host(short: &str) -> bool {
    matches!(short, "dload" | "declare" | "invoke") || short.starts_with("ffi_")
}

/// Factories, `open`/`read`/`write*`/`close`, and tcp/udp/fs_* HostInvoke.
fn is_fd_endpoint(short: &str) -> bool {
    fd_factory_name(short).is_some() || is_fd_op(short)
}

fn fd_arg_name(
    args: Option<&[Output<'_>]>,
    aliases: &HashMap<String, EscapeKind>,
) -> Option<String> {
    let args = args?;
    let first = args.first()?;
    fd_expr_name(first, aliases)
}

fn fd_expr_name(expr: &Output<'_>, aliases: &HashMap<String, EscapeKind>) -> Option<String> {
    let expr = peel(expr);
    let expr = match expr.1.as_ref() {
        Expression::Try(inner) => peel(inner),
        _ => expr,
    };
    if let Some(n) = ident_name(expr) {
        return Some(n.to_string());
    }
    if let Expression::Call { name, args } = expr.1.as_ref() {
        let callee = callee_name(name)?;
        let short = callee.rsplit("::").next().unwrap_or(callee);
        if let Some(factory) = fd_factory_name(short) {
            return Some(factory.to_string());
        }
        if is_fd_op(short) {
            return fd_arg_name(args.as_deref(), aliases).or_else(|| Some(short.to_string()));
        }
    }
    None
}

fn is_fd_op(short: &str) -> bool {
    matches!(
        short,
        "write"
            | "write_all"
            | "write_from"
            | "read"
            | "read_to_end"
            | "open"
            | "close"
            | "connect"
            | "connect_timeout"
            | "listen"
            | "accept"
            | "bind"
            | "send_to"
            | "recv_from"
            | "shutdown"
            | "await_readable"
            | "await_writable"
            | "wait_readable"
            | "wait_writable"
            | "drive"
            | "wait_ready"
            | "set_nodelay"
            | "peer_addr"
            | "local_addr"
            | "local_port"
            | "connect_tls"
    ) || short.starts_with("tcp_")
        || short.starts_with("udp_")
        || short.starts_with("fs_")
}

fn is_mutex_host(short: &str) -> bool {
    matches!(
        short,
        "mutex"
            | "with_lock"
            | "lock"
            | "try_lock"
            | "unlock"
            | "rwlock"
            | "with_read"
            | "with_write"
            | "try_read"
            | "try_write"
    )
}

fn first_ident_arg<'a>(args: Option<&'a [Output<'a>]>) -> Option<&'a str> {
    let args = args?;
    let first = args.first()?;
    ident_name(first).or_else(|| {
        if let Expression::Call { name, .. } = peel(first).1.as_ref() {
            callee_name(name).and_then(fd_factory_name)
        } else {
            None
        }
    })
}

/// `with_lock(R, callback)` whose callback contains a would-be call bag.
fn covering_lock_bag(
    body: &Output<'_>,
    user_fns: &HashSet<String>,
) -> Option<(Range<usize>, NamedEscape)> {
    find_covering_lock(body, user_fns)
}

fn find_covering_lock(
    ast: &Output<'_>,
    user_fns: &HashSet<String>,
) -> Option<(Range<usize>, NamedEscape)> {
    match ast.1.as_ref() {
        Expression::Program(items)
        | Expression::Block(items)
        | Expression::Fragment(items)
        | Expression::Array(items)
        | Expression::Tuple(items)
        | Expression::If(items) => {
            for item in items {
                if let Some(hit) = find_covering_lock(item, user_fns) {
                    return Some(hit);
                }
            }
            None
        }
        Expression::Call {
            name,
            args: Some(args),
        } => {
            if let Some(hit) = with_lock_if_bag(name, args, user_fns) {
                return Some(hit);
            }
            find_covering_lock(name, user_fns)
                .or_else(|| args.iter().find_map(|a| find_covering_lock(a, user_fns)))
        }
        Expression::Call { name, args: None } => find_covering_lock(name, user_fns),
        Expression::Statement(inner)
        | Expression::Expr(inner)
        | Expression::ExprStatement(inner)
        | Expression::Group(inner)
        | Expression::Return(inner)
        | Expression::ImplicitReturn(inner)
        | Expression::Try(inner)
        | Expression::Lambda { body: inner, .. } => find_covering_lock(inner, user_fns),
        Expression::Add(a, b)
        | Expression::Sub(a, b)
        | Expression::Mul(a, b)
        | Expression::Assignment(a, b) => {
            find_covering_lock(a, user_fns).or_else(|| find_covering_lock(b, user_fns))
        }
        Expression::Branch(cond, body) => cond
            .as_ref()
            .and_then(|c| find_covering_lock(c, user_fns))
            .or_else(|| find_covering_lock(body, user_fns)),
        Expression::Match { scrutinee, arms } => {
            find_covering_lock(scrutinee, user_fns).or_else(|| {
                arms.iter()
                    .find_map(|a| find_covering_lock(&a.body, user_fns))
            })
        }
        Expression::Function {
            body: Some(body), ..
        } => find_covering_lock(body, user_fns),
        Expression::NamedArg(_, v) => find_covering_lock(v, user_fns),
        _ => None,
    }
}

fn with_lock_if_bag(
    name: &Output<'_>,
    args: &[Output<'_>],
    user_fns: &HashSet<String>,
) -> Option<(Range<usize>, NamedEscape)> {
    let callee = callee_name(name)?;
    let short = callee.rsplit("::").next().unwrap_or(callee);
    if !matches!(short, "with_lock" | "with_read" | "with_write") {
        return None;
    }
    let resource = first_ident_arg(Some(args)).unwrap_or(short);
    let callback = args.get(1)?;
    if !contains_call_bag(callback, user_fns) {
        return None;
    }
    Some((
        name.0.into_range(),
        NamedEscape {
            kind: EscapeKind::Mutex,
            name: resource.to_string(),
        },
    ))
}

fn contains_call_bag(ast: &Output<'_>, user_fns: &HashSet<String>) -> bool {
    if is_call_bag(ast, user_fns) {
        return true;
    }
    match ast.1.as_ref() {
        Expression::Program(items)
        | Expression::Block(items)
        | Expression::Fragment(items)
        | Expression::Array(items)
        | Expression::Tuple(items)
        | Expression::If(items) => items.iter().any(|i| contains_call_bag(i, user_fns)),
        Expression::Statement(inner)
        | Expression::Expr(inner)
        | Expression::ExprStatement(inner)
        | Expression::Group(inner)
        | Expression::Return(inner)
        | Expression::ImplicitReturn(inner)
        | Expression::Try(inner)
        | Expression::Lambda { body: inner, .. }
        | Expression::Function {
            body: Some(inner), ..
        } => contains_call_bag(inner, user_fns),
        Expression::Add(a, b)
        | Expression::Sub(a, b)
        | Expression::Mul(a, b)
        | Expression::Xor(a, b)
        | Expression::Assignment(a, b) => {
            contains_call_bag(a, user_fns) || contains_call_bag(b, user_fns)
        }
        Expression::Call { name, args } => {
            contains_call_bag(name, user_fns)
                || args
                    .iter()
                    .flatten()
                    .any(|a| contains_call_bag(a, user_fns))
        }
        Expression::Branch(cond, body) => {
            cond.as_ref()
                .is_some_and(|c| contains_call_bag(c, user_fns))
                || contains_call_bag(body, user_fns)
        }
        Expression::Construct { fields, .. } => match fields {
            EnumConstructPayload::Tuple(items) => {
                items.iter().any(|i| contains_call_bag(i, user_fns))
            }
            EnumConstructPayload::Record(fields) => {
                fields.iter().any(|f| contains_call_bag(&f.value, user_fns))
            }
            EnumConstructPayload::Unit => false,
        },
        Expression::NamedArg(_, v) => contains_call_bag(v, user_fns),
        _ => false,
    }
}

fn is_call_bag(expr: &Output<'_>, user_fns: &HashSet<String>) -> bool {
    let expr = peel(expr);
    let mut leaves = Vec::new();
    match expr.1.as_ref() {
        Expression::Add(_, _) => flatten_assoc(expr, AssocOp::Add, &mut leaves),
        Expression::Mul(_, _) => flatten_assoc(expr, AssocOp::Mul, &mut leaves),
        Expression::Xor(_, _) => flatten_assoc(expr, AssocOp::Xor, &mut leaves),
        Expression::Sub(a, b) => {
            return is_user_call(a, user_fns) && is_user_call(b, user_fns);
        }
        _ => return false,
    }
    leaves.len() >= 2 && leaves.iter().all(|l| is_user_call(l, user_fns))
}

#[derive(Clone, Copy)]
enum AssocOp {
    Add,
    Mul,
    Xor,
}

fn flatten_assoc<'a>(expr: &'a Output<'a>, op: AssocOp, out: &mut Vec<&'a Output<'a>>) {
    let expr = peel(expr);
    let nested = match (op, expr.1.as_ref()) {
        (AssocOp::Add, Expression::Add(a, b))
        | (AssocOp::Mul, Expression::Mul(a, b))
        | (AssocOp::Xor, Expression::Xor(a, b)) => Some((a, b)),
        _ => None,
    };
    if let Some((a, b)) = nested {
        flatten_assoc(a, op, out);
        flatten_assoc(b, op, out);
    } else {
        out.push(expr);
    }
}

fn is_user_call(expr: &Output<'_>, user_fns: &HashSet<String>) -> bool {
    let Expression::Call { name, .. } = peel(expr).1.as_ref() else {
        return false;
    };
    callee_name(name).is_some_and(|n| {
        let short = n.rsplit("::").next().unwrap_or(n);
        user_fns.contains(n) || user_fns.contains(short)
    })
}

fn callee_name<'a>(name: &'a Output<'a>) -> Option<&'a str> {
    match peel(name).1.as_ref() {
        Expression::Identifier(n) => Some(*n),
        Expression::QualifiedAccess { owner, member } => {
            // Best-effort: purity stores `owner::member`; we only need the member
            // for host classification.
            let _ = owner;
            Some(*member)
        }
        _ => None,
    }
}

fn ident_name<'a>(expr: &'a Output<'a>) -> Option<&'a str> {
    match peel(expr).1.as_ref() {
        Expression::Identifier(n) => Some(*n),
        _ => None,
    }
}

fn peel<'a>(expr: &'a Output<'a>) -> &'a Output<'a> {
    match expr.1.as_ref() {
        Expression::Expr(inner)
        | Expression::Group(inner)
        | Expression::Statement(inner)
        | Expression::ExprStatement(inner) => peel(inner),
        Expression::Fragment(items) if items.len() == 1 => peel(&items[0]),
        _ => expr,
    }
}
