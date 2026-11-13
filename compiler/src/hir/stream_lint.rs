//! E0148: a local `Stream` a body opens and never closes or hands on.
//!
//! The collector closes a stream it finds unreachable, and `open` /
//! `accept` collect once and retry when the process is out of descriptors,
//! but a loop that opens faster than the collector runs can still run out.
//! This pass names the plain case: `let s = open(…)` (or `connect`,
//! `listen`, `accept`, a UDP `bind` / `connect`), where every later use of
//! `s` is an `io` call other than `close`. Passing `s` to any other
//! function, returning it, storing it, matching on it or capturing it in a
//! lambda hands it on, and the pass stays quiet. A `close(s)` anywhere in
//! the body, a `defer` one included, counts: the pass does not follow paths.

use std::collections::{HashMap, HashSet};

use reporting::{ErrorCode, Message};

use crate::hir::{Callee, HirBody, HirId, HirKind, LocalId, LocalKind};
use crate::typechecking::infer::Checker;
use crate::typechecking::ty::{STREAM, Ty, strip_readonly};
use crate::typechecking::virtual_modules::IoBuiltin;

/// The `io` native a call runs, when it is one.
fn io_call(checker: &Checker, callee: &Callee) -> Option<IoBuiltin> {
    let Callee::Named { name, def: None, .. } = callee else { return None };
    checker
        .io_fn_in_scope(name)
        .or_else(|| name.strip_prefix("io::").and_then(|_| checker.io_fn_in_scope(name.rsplit("::").next()?)))
}

/// Whether `ty` is `Stream` (or `readonly Stream`).
fn is_stream(ty: Option<&Ty>) -> bool {
    matches!(ty.map(strip_readonly), Some(Ty::Con(n)) if n == STREAM)
}

/// Whether `init` opens a new descriptor: a call to an opening `io` native,
/// behind the `match` a `?` builds or an `.unwrap()` / `.expect(…)`.
fn opens(checker: &Checker, body: &HirBody, init: HirId) -> bool {
    match &body.expr(init).kind {
        HirKind::Match { scrutinee, .. } => opens(checker, body, *scrutinee),
        HirKind::Call { callee: Callee::Method { name }, args } if matches!(name.as_str(), "unwrap" | "expect") => {
            args.first().is_some_and(|&a| opens(checker, body, a))
        }
        HirKind::Call { callee, .. } => matches!(
            io_call(checker, callee),
            Some(
                IoBuiltin::Open
                    | IoBuiltin::TcpConnect
                    | IoBuiltin::TcpConnectTimeout
                    | IoBuiltin::TcpListen
                    | IoBuiltin::TcpAccept
                    | IoBuiltin::UdpBind
                    | IoBuiltin::UdpConnect
            )
        ),
        _ => false,
    }
}

/// What one read of a tracked local does with the stream.
enum Use {
    /// `close(s)`.
    Close,
    /// An `io` call or `Stream` method that leaves the stream open.
    Keep,
    /// `let t = s`: `t` names the same stream.
    Alias(LocalId),
    /// Anything else: the stream leaves this body's hands.
    HandOff,
}

fn use_of(checker: &Checker, body: &HirBody, parent: Option<HirId>, node: HirId) -> Use {
    let Some(p) = parent else { return Use::HandOff };
    match &body.expr(p).kind {
        HirKind::Call { callee: callee @ Callee::Named { .. }, .. } => match io_call(checker, callee) {
            Some(IoBuiltin::Close) => Use::Close,
            Some(_) => Use::Keep,
            None => Use::HandOff,
        },
        HirKind::Call { callee: Callee::Method { name }, args }
            if args.first() == Some(&node) && matches!(name.as_str(), "fd" | "attach" | "park") =>
        {
            Use::Keep
        }
        HirKind::Let { local, init: Some(i) } if *i == node => Use::Alias(*local),
        _ => Use::HandOff,
    }
}

/// E0148 warnings for `body`.
pub(crate) fn check(checker: &Checker, module: &crate::hir::HirModule, body: &HirBody) -> Vec<Message> {
    let Some(root) = body.root else { return Vec::new() };
    let mut parent: HashMap<HirId, HirId> = HashMap::new();
    let mut lets: Vec<(LocalId, HirId, HirId)> = Vec::new();
    let mut reads: HashMap<LocalId, Vec<HirId>> = HashMap::new();
    let mut lambda_captured: HashSet<LocalId> = HashSet::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        match &body.expr(id).kind {
            HirKind::Local(l) => reads.entry(*l).or_default().push(id),
            HirKind::Let { local, init: Some(i) } => lets.push((*local, id, *i)),
            HirKind::Lambda { body: inner } => {
                if let Some(b) = module.bodies.get(*inner) {
                    lambda_captured.extend(b.captures.iter().map(|(o, _)| *o));
                }
            }
            _ => {}
        }
        for k in body.children(id) {
            parent.insert(k, id);
            stack.push(k);
        }
    }
    let mut out = Vec::new();
    for &(l, at, init) in &lets {
        let local = body.local(l);
        if local.kind != LocalKind::Let
            || local.name.starts_with('$')
            || local.name.starts_with('_')
            || !is_stream(local.ty.as_ref())
            || !opens(checker, body, init)
        {
            continue;
        }
        // Follow the stream through aliases; stop at a close or a hand-off.
        let mut names = vec![l];
        let mut seen: HashSet<LocalId> = HashSet::new();
        let mut settled = false;
        while let Some(n) = names.pop() {
            if !seen.insert(n) {
                continue;
            }
            if lambda_captured.contains(&n) {
                settled = true;
                break;
            }
            for &r in reads.get(&n).map(Vec::as_slice).unwrap_or(&[]) {
                match use_of(checker, body, parent.get(&r).copied(), r) {
                    Use::Keep => {}
                    Use::Alias(t) => names.push(t),
                    Use::Close | Use::HandOff => settled = true,
                }
            }
            if settled {
                break;
            }
        }
        if settled {
            continue;
        }
        let name = &local.name;
        let span = body.expr(at).span;
        let mut m = Message::warn(
            ErrorCode::UnclosedStream,
            format!("`{name}` is opened here and never closed: its descriptor stays open until the collector finds it"),
            span.0..span.1,
        );
        m.with_help(format!(
            "close it with `close({name})`, or right after the open write `defer use ({name}) {{ let _ = close({name}); }}`"
        ));
        out.push(m);
    }
    out
}
