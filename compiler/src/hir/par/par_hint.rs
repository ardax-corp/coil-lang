//! F3 parallelization hints: would-be IPA call bags refused on shared escapes.
//!
//! Does **not** insert locks or emit workers. See
//! [`docs/internals/par-lock-hints.md`](../../../../docs/internals/par-lock-hints.md).
//!
//! The walk runs on a module's HIR as built. It reads calls as they were
//! written: a resource is named by the local or the callee at the call
//! (`stdout`, `f`, `m`), and a host is classified by its short name.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::Range;


use super::super::{Builtin, Callee, HirBody, HirFlags, HirId, HirKind, HirModule, MakeKind, query};
use super::loop_par::source_op;
use super::par_profit::{Made, bare, in_contract, made_match, returned};
use crate::typechecking::infer::Checker;

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

fn format_edges(resources: &[NamedEscape]) -> String {
    resources
        .iter()
        .map(|r| format!("`{}` ({})", r.name, r.kind.as_label()))
        .collect::<Vec<_>>()
        .join(", ")
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
    #[allow(dead_code)] // unit test `unlocked_stdout_recursive_bag_is_named`
    pub fn primary_resource(&self) -> Option<&NamedEscape> {
        self.resources.first()
    }

    /// `` `stdout` (FD), `f` (FD) `` — every lockable edge, not only the first.
    pub fn edges_text(&self) -> String {
        format_edges(&self.resources)
    }

    /// Locked hint text (COI-369). Lists **all** named escapes on the bag.
    pub fn message(&self) -> String {
        let edges = self.edges_text();
        if edges.is_empty() {
            return format!(
                "this call bag in `{}` would be parallelizable if a shared escape is locked across the parallel region",
                self.fn_name
            );
        }
        if self.covering_lock {
            let lock = self
                .resources
                .iter()
                .find(|r| r.kind == EscapeKind::Mutex)
                .or_else(|| self.resources.first());
            let lock_txt = lock
                .map(|r| format!("`{}` ({})", r.name, r.kind.as_label()))
                .unwrap_or_else(|| edges.clone());
            format!(
                "covering lock on {lock_txt} around the call bag in `{}`; lockable edges {edges}; shared steal stays sequential this cut (missing lock remains today's refuse)",
                self.fn_name
            )
        } else {
            format!(
                "this call bag in `{}` would be parallelizable if resource {edges} is locked across the parallel region",
                self.fn_name
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

impl HirModule {
    /// Would-be IPA sites refused because a lockable shared escape is in
    /// the bag.
    ///
    /// Uses the same fork-shape detector as F0–F2
    /// ([`Self::par_fork_sites`]), but with **all** user `fn`s as candidate
    /// arms. Pure functions that already IPA are skipped. Panic / yield
    /// impurity without a lockable resource is skipped.
    ///
    /// `impure` says, for each short name among the module's functions and
    /// methods, whether a body of that name is impure
    /// ([`crate::hir::effects::impure_short_names`]).
    pub fn par_escape_hints(&self, checker: &Checker, impure: &HashMap<String, bool>) -> Vec<ParEscapeHint> {
        let user_fns: HashSet<String> = impure.keys().cloned().collect();
        if user_fns.is_empty() {
            return Vec::new();
        }
        let sites = self.par_fork_sites(checker, &user_fns);
        // A later function of the same bare name replaces an earlier one.
        let bodies: HashMap<&str, &HirBody> = self.fork_bodies().into_iter().map(|b| (bare(&b.name), b)).collect();
        let cx = Cx { module: self, checker, user_fns: &user_fns, bodies: &bodies };
        let impure = |name: &str| impure.get(name).copied().unwrap_or(false);

        let mut out = Vec::new();
        let mut hinted = HashSet::new();
        for name in sites.keys() {
            if !impure(name) {
                continue;
            }
            let Some(body) = bodies.get(name.as_str()) else { continue };
            let Some(root) = body.root else { continue };
            let resources = cx.named_escapes_in_fn(name);
            if resources.is_empty() {
                continue;
            }
            hinted.insert(name.as_str());
            let span = body.expr(root).span;
            out.push(ParEscapeHint { fn_name: name.clone(), span: span.0..span.1, resources, covering_lock: false });
        }

        // Call bags only inside `with_lock` callbacks are not named-fn fork sites.
        for (&name, body) in &bodies {
            if hinted.contains(name) || !impure(name) {
                continue;
            }
            if let Some((span, lock)) = cx.covering_lock_bag(body) {
                let mut resources = cx.named_escapes_in_fn(name);
                if !resources.contains(&lock) {
                    resources.insert(0, lock);
                }
                out.push(ParEscapeHint { fn_name: name.to_string(), span, resources, covering_lock: true });
            }
        }

        // If a named-fn site also has a covering lock wrapping a bag, upgrade.
        for hint in &mut out {
            if hint.covering_lock {
                continue;
            }
            if let Some(body) = bodies.get(hint.fn_name.as_str())
                && let Some((_, resource)) = cx.covering_lock_bag(body)
            {
                hint.covering_lock = true;
                if !hint.resources.contains(&resource) {
                    hint.resources.insert(0, resource);
                }
            }
        }

        out.sort_by(|a, b| a.fn_name.cmp(&b.fn_name));
        out
    }
}

struct Cx<'a> {
    module: &'a HirModule,
    checker: &'a Checker,
    user_fns: &'a HashSet<String>,
    /// Functions and inherent methods by bare name.
    bodies: &'a HashMap<&'a str, &'a HirBody>,
}

/// A call as it was written: `f(…)` or `m::f(…)`.
struct Written<'a> {
    /// The function's own name (`f`), which classifies a host.
    short: &'a str,
    /// Where the callee is written: the name of a bare call, the whole
    /// call for a path.
    span: Range<usize>,
}

impl<'a> Cx<'a> {
    /// The callee of call `id`, when the source names one: a bare name or a
    /// lower-case module path (`io::write`). A method call, a call through
    /// a trait and `Type::f(…)` (which the parser reads as a construction)
    /// name none, and neither does a call the builder made.
    fn written_call(&self, body: &'a HirBody, id: HirId) -> Option<Written<'a>> {
        let e = body.expr(id);
        let HirKind::Call { callee, .. } = &e.kind else { return None };
        e.node?;
        let lower = |s: &str| s.chars().next().is_some_and(|c| c.is_ascii_lowercase());
        let name = match callee {
            Callee::Named { name, .. } => name.as_str(),
            // A local or a `const` holding a function, called by its name.
            Callee::Value(v) => ident_name(body, *v)?,
            _ => return None,
        };
        if self.checker.qualified_trait_call_at(e.span.0, e.span.1).is_some() {
            return None;
        }
        let mut path = name.rsplit("::");
        let short = path.next()?;
        match path.next() {
            None => Some(Written { short, span: e.span.0..e.span.0 + name.len() }),
            Some(owner) => (lower(owner) && lower(short)).then_some(Written { short, span: e.span.0..e.span.1 }),
        }
    }

    fn user_call(&self, body: &HirBody, id: HirId) -> bool {
        self.written_call(body, id).is_some_and(|c| self.user_fns.contains(c.short))
    }

    /// The nodes of `id` a walk looks into, in source order, with a
    /// lambda's body as `Err(body)`. `None` for a
    /// node the AST walk this one replaced did not enter.
    fn parts(&self, body: &'a HirBody, id: HirId) -> Option<Vec<Result<HirId, &'a HirBody>>> {
        let e = body.expr(id);
        if in_contract(body, id) {
            return None;
        }
        if let Some(made) = made_match(body, id) {
            return Some(match made {
                Made::Try(inner) | Made::MainReturn(inner) => vec![Ok(inner)],
                Made::Coalesce(lhs, rhs) => vec![Ok(lhs), Ok(rhs)],
                // `e?.f` is its base.
                Made::Other => body.children(id).take(1).map(Ok).collect(),
            });
        }
        match &e.kind {
            HirKind::Lambda { body: index } => Some(self.module.bodies.get(*index).into_iter().map(Err).collect()),
            // A `while let` is not entered.
            HirKind::Loop { body: loop_body } => {
                let (_, cond, then) = query::while_parts(body, *loop_body)?;
                Some(vec![Ok(cond), Ok(then)])
            }
            HirKind::Make { kind: MakeKind::Class(_), .. } | HirKind::Yield { .. } | HirKind::Resume { .. } | HirKind::Spread(_) => None,
            HirKind::Builtin { op, .. } if !matches!(op, Builtin::TypeOf | Builtin::Dload | Builtin::Declare | Builtin::Invoke) => None,
            _ => Some(body.children(id).map(Ok).collect()),
        }
    }

    fn named_escapes_in_fn(&self, fn_name: &str) -> Vec<NamedEscape> {
        let mut found = BTreeSet::new();
        let mut stack = vec![fn_name.to_string()];
        let mut seen = HashSet::new();
        while let Some(name) = stack.pop() {
            if !seen.insert(name.clone()) {
                continue;
            }
            if let Some(body) = self.bodies.get(name.as_str())
                && let Some(root) = body.root
            {
                self.walk_escapes(body, root, &mut found, &mut stack);
            }
        }
        found.into_iter().collect()
    }

    fn walk_escapes(&self, body: &'a HirBody, id: HirId, out: &mut BTreeSet<NamedEscape>, callees: &mut Vec<String>) {
        // Walks recurse through here once per level of the tree.
        common::deep_stack(|| self.walk_escapes_level(body, id, out, callees))
    }

    fn walk_escapes_level(&self, body: &'a HirBody, id: HirId, out: &mut BTreeSet<NamedEscape>, callees: &mut Vec<String>) {
        let Some(parts) = self.parts(body, id) else { return };
        let heap = |place: HirId, out: &mut BTreeSet<NamedEscape>| {
            if let HirKind::Index { base, .. } | HirKind::Field { base, .. } = &body.expr(place).kind
                && let Some(name) = ident_name(body, *base)
            {
                out.insert(NamedEscape { kind: EscapeKind::HeapObject, name: name.to_string() });
            }
        };
        match &body.expr(id).kind {
            // The left of an operator or an assignment, when it is an
            // element or a field of a named object.
            HirKind::Bin { op, lhs, .. } if !matches!(source_op(*op), "<<" | ">>") => heap(*lhs, out),
            HirKind::Logic { lhs, .. } => heap(*lhs, out),
            HirKind::Assign { place, .. } => heap(*place, out),
            HirKind::Append { base, .. } => {
                if let Some(name) = ident_name(body, *base) {
                    out.insert(NamedEscape { kind: EscapeKind::HeapObject, name: name.to_string() });
                }
            }
            HirKind::Match { scrutinee, .. } if matches!(made_match(body, id), Some(Made::Coalesce(..))) => heap(*scrutinee, out),
            HirKind::Call { args, .. } => self.note_call_escape(body, id, args, out, callees),
            HirKind::Builtin { op: Builtin::Dload, .. } => note_ffi_names(body, "dload", &[], out),
            HirKind::Builtin { op: Builtin::Declare, args } => note_ffi_names(body, "declare", args, out),
            HirKind::Builtin { op: Builtin::Invoke, args } => note_ffi_names(body, "invoke", args, out),
            _ => {}
        }
        for part in parts {
            match part {
                Ok(k) => self.walk_escapes(body, k, out, callees),
                Err(inner) => {
                    if let Some(root) = inner.root {
                        self.walk_escapes(inner, root, out, callees);
                    }
                }
            }
        }
    }

    fn note_call_escape(&self, body: &'a HirBody, id: HirId, args: &[HirId], out: &mut BTreeSet<NamedEscape>, callees: &mut Vec<String>) {
        let Some(Written { short, .. }) = self.written_call(body, id) else { return };
        if self.user_fns.contains(short) {
            callees.push(short.to_string());
            return;
        }
        if is_format_like(short) {
            return;
        }
        if is_fd_endpoint(short) {
            let name = self.fd_arg_name(body, args).unwrap_or_else(|| short.to_string());
            out.insert(NamedEscape { kind: EscapeKind::Fd, name });
            return;
        }
        if is_ffi_host(short) {
            note_ffi_names(body, short, args, out);
            return;
        }
        if is_mutex_host(short) && !matches!(short, "mutex" | "rwlock") {
            let name = self.first_ident_arg(body, args).unwrap_or(short);
            out.insert(NamedEscape { kind: EscapeKind::Mutex, name: name.to_string() });
        }
    }

    /// The name of the stream a call's first argument is: a local, a
    /// factory (`stdout()`), or the stream another stream call takes.
    fn fd_arg_name(&self, body: &'a HirBody, args: &[HirId]) -> Option<String> {
        let first = *args.first()?;
        let first = match made_match(body, first) {
            Some(Made::Try(inner)) => inner,
            _ => first,
        };
        if let Some(name) = ident_name(body, first) {
            return Some(name.to_string());
        }
        let HirKind::Call { args, .. } = &body.expr(first).kind else { return None };
        let short = self.written_call(body, first)?.short;
        if let Some(factory) = fd_factory_name(short) {
            return Some(factory.to_string());
        }
        is_fd_op(short).then(|| self.fd_arg_name(body, args).unwrap_or_else(|| short.to_string()))
    }

    fn first_ident_arg(&self, body: &'a HirBody, args: &[HirId]) -> Option<&'a str> {
        let first = *args.first()?;
        ident_name(body, first).or_else(|| fd_factory_name(self.written_call(body, first)?.short))
    }

    /// `with_lock(R, callback)` whose callback contains a would-be call bag.
    fn covering_lock_bag(&self, body: &'a HirBody) -> Option<(Range<usize>, NamedEscape)> {
        self.find_covering_lock(body, body.root?)
    }

    fn find_covering_lock(&self, body: &'a HirBody, id: HirId) -> Option<(Range<usize>, NamedEscape)> {
        if let HirKind::Call { args, .. } = &body.expr(id).kind
            && let Some(hit) = self.with_lock_if_bag(body, id, args)
        {
            return Some(hit);
        }
        self.bag_parts(body, id, false).into_iter().find_map(|part| match part {
            Ok(k) => self.find_covering_lock(body, k),
            Err(inner) => self.find_covering_lock(inner, inner.root?),
        })
    }

    fn with_lock_if_bag(&self, body: &'a HirBody, id: HirId, args: &[HirId]) -> Option<(Range<usize>, NamedEscape)> {
        let call = self.written_call(body, id)?;
        if !matches!(call.short, "with_lock" | "with_read" | "with_write") {
            return None;
        }
        let resource = self.first_ident_arg(body, args).unwrap_or(call.short);
        let callback = *args.get(1)?;
        if !self.contains_call_bag(body, callback) {
            return None;
        }
        Some((call.span, NamedEscape { kind: EscapeKind::Mutex, name: resource.to_string() }))
    }

    fn contains_call_bag(&self, body: &'a HirBody, id: HirId) -> bool {
        self.is_call_bag(body, id)
            || self.bag_parts(body, id, true).into_iter().any(|part| match part {
                Ok(k) => self.contains_call_bag(body, k),
                Err(inner) => inner.root.is_some_and(|root| self.contains_call_bag(inner, root)),
            })
    }

    /// The nodes the covering-lock walks look into: statement lists, `if`,
    /// `return`, `?`, lambdas, `+ - *` and assignments, call arguments;
    /// `match` arms when looking for the lock, `^` and constructor
    /// arguments when looking for the bag under it.
    fn bag_parts(&self, body: &'a HirBody, id: HirId, in_callback: bool) -> Vec<Result<HirId, &'a HirBody>> {
        let Some(parts) = self.parts(body, id) else { return Vec::new() };
        let e = body.expr(id);
        let enters = match &e.kind {
            HirKind::Block { .. } | HirKind::Let { .. } | HirKind::If { .. } | HirKind::Lambda { .. } | HirKind::Named { .. } => true,
            // Not a `raise`.
            HirKind::Return(_) => returned(body, id).is_some(),
            HirKind::Make { kind: MakeKind::Array | MakeKind::Tuple, .. } => true,
            // The `Ok(…)` the builder wraps a returned value in is the value.
            HirKind::Make { kind: MakeKind::Variant { .. }, .. } => in_callback || e.node.is_none(),
            HirKind::Match { .. } => match made_match(body, id) {
                Some(Made::Try(_) | Made::MainReturn(_)) => true,
                Some(_) => false,
                None => !in_callback,
            },
            HirKind::Bin { op, .. } => matches!(source_op(*op), "+" | "-" | "*") || (in_callback && source_op(*op) == "^"),
            HirKind::Assign { .. } => !e.flags.contains(HirFlags::COMPOUND),
            // A method's receiver is not looked into.
            HirKind::Call { callee: Callee::Method { .. }, .. } => return parts.into_iter().skip(1).collect(),
            HirKind::Call { .. } => true,
            _ => false,
        };
        if enters { parts } else { Vec::new() }
    }

    /// `f(…) ⊕ g(…)` over user functions: two or more for `+ * ^`, two for `-`.
    fn is_call_bag(&self, body: &'a HirBody, id: HirId) -> bool {
        let HirKind::Bin { op, lhs, rhs } = body.expr(id).kind else { return false };
        let op = source_op(op);
        match op {
            "-" => self.user_call(body, lhs) && self.user_call(body, rhs),
            "+" | "*" | "^" => {
                let mut leaves = Vec::new();
                flatten_assoc(body, id, op, &mut leaves);
                leaves.len() >= 2 && leaves.iter().all(|l| self.user_call(body, *l))
            }
            _ => false,
        }
    }
}

fn flatten_assoc(body: &HirBody, id: HirId, op: &str, out: &mut Vec<HirId>) {
    match body.expr(id).kind {
        HirKind::Bin { op: found, lhs, rhs } if source_op(found) == op => {
            flatten_assoc(body, lhs, op, out);
            flatten_assoc(body, rhs, op, out);
        }
        _ => out.push(id),
    }
}

/// The name an expression is written as: a local, or a bare global.
fn ident_name(body: &HirBody, id: HirId) -> Option<&str> {
    match &body.expr(id).kind {
        HirKind::Local(l) => Some(body.local(*l).name.as_str()),
        HirKind::Global { name, .. } if !name.contains("::") => Some(name.as_str()),
        _ => None,
    }
}

fn note_ffi_names(body: &HirBody, short: &str, args: &[HirId], out: &mut BTreeSet<NamedEscape>) {
    out.insert(NamedEscape { kind: EscapeKind::Ffi, name: short.to_string() });
    if let Some(name) = args.first().and_then(|first| ident_name(body, *first))
        && name != short
    {
        out.insert(NamedEscape { kind: EscapeKind::Ffi, name: name.to_string() });
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

#[cfg(test)]
#[path = "par_hint.tests.rs"]
mod tests;
