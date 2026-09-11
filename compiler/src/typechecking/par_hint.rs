//! F3 parallelization hints: would-be IPA call bags refused on shared escapes.
//!
//! Does **not** insert locks or emit workers. See
//! [`docs/internals/par-lock-hints.md`](../../../docs/internals/par-lock-hints.md).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::Range;

use parser::ast::{EnumConstructPayload, Expression, Output};

use super::par_profit::analyze_par_fork_sites;
use super::purity::{EffectFlags, analyze_fn_effects, classify_host_name};

/// Kind of shared resource a covering userland lock would name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EscapeKind {
    Fd,
    Ffi,
    Mutex,
    HeapObject,
}

impl EscapeKind {
    pub fn as_label(self) -> &'static str {
        match self {
            Self::Fd => "FD",
            Self::Ffi => "FFI handle",
            Self::Mutex => "mutex",
            Self::HeapObject => "user object",
        }
    }
}

/// One named shared escape on a would-be call bag.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct NamedEscape {
    pub kind: EscapeKind,
    pub name: String,
}

/// Compile-time hint: this chunk would be parallelizable if `R` is locked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParEscapeHint {
    pub fn_name: String,
    pub span: Range<usize>,
    pub resources: Vec<NamedEscape>,
    /// `with_lock(R, …)` wraps the call bag. Still sequential this cut.
    pub covering_lock: bool,
}

impl ParEscapeHint {
    pub fn primary_resource(&self) -> Option<&NamedEscape> {
        self.resources.first()
    }

    /// Locked hint text (COI-369). Architect may discard the wording.
    pub fn message(&self) -> String {
        let Some(r) = self.primary_resource() else {
            return format!(
                "this call bag in `{}` would be parallelizable if a shared escape is locked across the parallel region",
                self.fn_name
            );
        };
        if self.covering_lock {
            format!(
                "covering lock on `{}` ({}) around the call bag in `{}`; shared steal stays sequential this cut (missing lock remains today's refuse)",
                r.name,
                r.kind.as_label(),
                self.fn_name
            )
        } else {
            format!(
                "this call bag in `{}` would be parallelizable if resource `{}` ({}) is locked across the parallel region",
                self.fn_name,
                r.name,
                r.kind.as_label()
            )
        }
    }

    pub fn help(&self) -> String {
        if self.covering_lock {
            "the compiler does not auto-fork on a covering lock yet; a later hold check / runtime assert may admit shared steal".into()
        } else {
            "take a covering lock in userland (e.g. `thread::with_lock`); the compiler does not insert locks or auto-fork on this hint".into()
        }
    }
}

/// Would-be IPA sites refused because a lockable shared escape is in the bag.
///
/// Uses the same fork-shape detector as F0–F2, but with **all** user `fn`s as
/// candidate arms. Pure functions that already IPA are skipped. Panic / yield
/// impurity without a lockable resource is skipped.
pub fn analyze_par_escape_hints(ast: &Output<'_>) -> Vec<ParEscapeHint> {
    let effects = analyze_fn_effects(ast);
    let user_fns: HashSet<String> = effects.keys().cloned().collect();
    if user_fns.is_empty() {
        return Vec::new();
    }
    let sites = analyze_par_fork_sites(ast, &user_fns);
    let mut bodies = HashMap::new();
    collect_fn_bodies(ast, &mut bodies);

    let mut out = Vec::new();
    let mut hinted = HashSet::new();

    for (name, site) in &sites {
        let flags = effects
            .get(name)
            .copied()
            .unwrap_or_else(EffectFlags::empty);
        if flags.is_pure() {
            continue;
        }
        if !flags.is_lockable_escape() {
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
        if flags.is_pure() || !flags.is_lockable_escape() {
            continue;
        }
        if let Some((span, resource)) = covering_lock_bag(body, &user_fns) {
            hinted.insert(name.clone());
            out.push(ParEscapeHint {
                fn_name: name.clone(),
                span,
                resources: vec![resource],
                covering_lock: true,
            });
        }
    }

    // If a named-fn site also has a covering lock wrapping a bag, upgrade.
    for hint in &mut out {
        if hint.covering_lock {
            continue;
        }
        if let Some(body) = bodies.get(hint.fn_name.as_str()) {
            if let Some((_, resource)) = covering_lock_bag(body, &user_fns) {
                hint.covering_lock = true;
                if !hint.resources.iter().any(|r| r.name == resource.name) {
                    hint.resources.insert(0, resource);
                }
            }
        }
    }

    out.sort_by(|a, b| a.fn_name.cmp(&b.fn_name));
    out
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
        walk_escapes(body, user_fns, &mut found, &mut stack);
    }
    found.into_iter().collect()
}

fn walk_escapes(
    ast: &Output<'_>,
    user_fns: &HashSet<String>,
    out: &mut BTreeSet<NamedEscape>,
    callees: &mut Vec<String>,
) {
    match ast.1.as_ref() {
        Expression::Program(items)
        | Expression::Block(items)
        | Expression::Fragment(items)
        | Expression::List(items)
        | Expression::Array(items)
        | Expression::Tuple(items)
        | Expression::If(items) => {
            for item in items {
                walk_escapes(item, user_fns, out, callees);
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
        | Expression::Member(inner) => walk_escapes(inner, user_fns, out, callees),
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
            walk_escapes(a, user_fns, out, callees);
            walk_escapes(b, user_fns, out, callees);
        }
        Expression::Call { name, args } => {
            note_call_escape(name, args.as_deref(), user_fns, out, callees);
            walk_escapes(name, user_fns, out, callees);
            for a in args.iter().flatten() {
                walk_escapes(a, user_fns, out, callees);
            }
        }
        Expression::Declare(_) | Expression::Invoke(_) => {
            out.insert(NamedEscape {
                kind: EscapeKind::Ffi,
                name: "invoke".into(),
            });
        }
        Expression::Construct { fields, .. } => match fields {
            EnumConstructPayload::Tuple(items) => {
                for item in items {
                    walk_escapes(item, user_fns, out, callees);
                }
            }
            EnumConstructPayload::Record(fields) => {
                for f in fields {
                    walk_escapes(&f.value, user_fns, out, callees);
                }
            }
            EnumConstructPayload::Unit => {}
        },
        Expression::Branch(cond, body) => {
            if let Some(c) = cond {
                walk_escapes(c, user_fns, out, callees);
            }
            walk_escapes(body, user_fns, out, callees);
        }
        Expression::Match { scrutinee, arms } => {
            walk_escapes(scrutinee, user_fns, out, callees);
            for arm in arms {
                walk_escapes(&arm.body, user_fns, out, callees);
            }
        }
        Expression::Loop {
            identifier,
            iterable,
            body,
        } => {
            if let Some(id) = identifier {
                walk_escapes(id, user_fns, out, callees);
            }
            walk_escapes(iterable, user_fns, out, callees);
            walk_escapes(body, user_fns, out, callees);
        }
        Expression::Variable(_, Some(init)) | Expression::Constant(_, Some(init)) => {
            walk_escapes(init, user_fns, out, callees)
        }
        Expression::LetDestructure { rhs, .. } => walk_escapes(rhs, user_fns, out, callees),
        Expression::Lambda { body, .. } => walk_escapes(body, user_fns, out, callees),
        Expression::Function {
            body: Some(body), ..
        } => walk_escapes(body, user_fns, out, callees),
        Expression::Index(base, Some(idx)) => {
            walk_escapes(base, user_fns, out, callees);
            walk_escapes(idx, user_fns, out, callees);
        }
        Expression::Index(base, None) | Expression::Access(base, _) => {
            walk_escapes(base, user_fns, out, callees)
        }
        Expression::NamedArg(_, v) => walk_escapes(v, user_fns, out, callees),
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
    let flags = classify_host_name(callee);
    if flags.contains(EffectFlags::IO) {
        if let Some(resource) = io_resource_name(short, args) {
            out.insert(NamedEscape {
                kind: EscapeKind::Fd,
                name: resource.to_string(),
            });
        }
        return;
    }
    if flags.contains(EffectFlags::FFI) {
        out.insert(NamedEscape {
            kind: EscapeKind::Ffi,
            name: short.to_string(),
        });
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

fn fd_factory_name(short: &str) -> Option<&'static str> {
    match short {
        "stdout" => Some("stdout"),
        "stderr" => Some("stderr"),
        "stdin" => Some("stdin"),
        _ => None,
    }
}

/// `format` / `to_bytes` are IO for purity but are not a lockable FD.
fn io_resource_name(short: &str, args: Option<&[Output<'_>]>) -> Option<String> {
    if matches!(short, "format" | "to_bytes" | "from_bytes") {
        return None;
    }
    if let Some(factory) = fd_factory_name(short) {
        return Some(factory.to_string());
    }
    if is_fd_op(short) {
        return Some(
            first_ident_arg(args)
                .map(str::to_string)
                .unwrap_or_else(|| short.to_string()),
        );
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
            | "open"
            | "close"
            | "connect"
            | "listen"
            | "accept"
            | "bind"
            | "send_to"
            | "recv_from"
            | "shutdown"
            | "await_readable"
            | "await_writable"
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
        | Expression::List(items)
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
        | Expression::List(items)
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

#[cfg(test)]
mod tests {
    use super::*;
    use parser::Pratt;

    fn parse(src: &'static str) -> parser::ast::Output<'static> {
        let owned = Box::leak(src.to_string().into_boxed_str());
        Pratt::default().parse(owned).expect("parse")
    }

    #[test]
    fn unlocked_stdout_recursive_bag_is_named() {
        let ast = parse(
            r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn leaf(int n) -> int {
    write(stdout(), to_bytes(format("%i", n)));
    return n;
}
fn rec(int n) -> int {
    if n <= 1 { return leaf(n); }
    return rec(n - 1) + rec(n - 2);
}
fn main() { return; }
"#,
        );
        let hints = analyze_par_escape_hints(&ast);
        let rec = hints.iter().find(|h| h.fn_name == "rec").expect("rec hint");
        assert!(!rec.covering_lock);
        assert!(
            rec.resources
                .iter()
                .any(|r| r.name == "stdout" && r.kind == EscapeKind::Fd),
            "named FD: {:?}",
            rec.resources
        );
        assert!(rec.message().contains("`stdout` (FD)"));
        assert!(hints.iter().all(|h| h.fn_name != "fib"));
    }

    #[test]
    fn covering_with_lock_documents_gate() {
        let ast = parse(
            r#"
use thread::{mutex, with_lock};
fn rec(Mutex m, int n) -> int {
    if n <= 1 { return 1; }
    return with_lock(m, fn (int x) => (rec(m, n - 1) + rec(m, n - 2), 0))?;
}
fn main() { return; }
"#,
        );
        let hints = analyze_par_escape_hints(&ast);
        let rec = hints.iter().find(|h| h.fn_name == "rec").expect("rec hint");
        assert!(rec.covering_lock, "{hints:?}");
        assert_eq!(rec.resources[0].name, "m");
        assert!(rec.message().contains("covering lock on `m`"));
    }

    #[test]
    fn pure_fib_has_no_escape_hint() {
        let ast = parse(
            r#"
fn fib(int n) -> int {
    if n <= 1 { return n; }
    return fib(n - 1) + fib(n - 2);
}
fn main() { return; }
"#,
        );
        let hints = analyze_par_escape_hints(&ast);
        assert!(hints.is_empty(), "pure IPA must not hint: {hints:?}");
    }

    #[test]
    fn panic_impurity_is_not_a_lock_hint() {
        let ast = parse(
            r#"
fn rec(int n) -> int {
    if n <= 1 { panic("x"); }
    return rec(n - 1) + rec(n - 2);
}
fn main() { return; }
"#,
        );
        let hints = analyze_par_escape_hints(&ast);
        assert!(hints.is_empty(), "panic is not lockable: {hints:?}");
    }
}
