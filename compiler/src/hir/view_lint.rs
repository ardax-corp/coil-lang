//! E0151: a view value outliving the borrows it holds.
//!
//! A `view class` holds borrows in its `mut` fields, so a view value is
//! second-class: it is the borrow, and lives no longer than the borrowed
//! place. It may be a temporary in a call chain, bound with `let`, passed as
//! an argument and returned by a method of the object it borrows, but this
//! pass refuses every way it could outlive the borrow:
//!
//! - a store into a field, an element, a static or a new object or variant;
//! - a type that holds one inside another (`Option<Entry>`, `Vec<Entry>`);
//! - a capture by a lambda (which a task or thread may run later);
//! - a return from anything but a method of the borrowed object (a view
//!   built from `mut self` or a trait method's `mut` receiver, `self` in a
//!   view's own method, or another such call on `self`).

use std::collections::HashSet;

use reporting::{ErrorCode, Message};

use crate::hir::{BodyKind, HirBody, HirId, HirKind, LocalKind, MakeKind};
use crate::typechecking::infer::Checker;
use crate::typechecking::ty::{Ty, strip_readonly};

/// The view class `ty` is an instance of, when it is one.
fn view_of(checker: &Checker, ty: Option<&Ty>) -> Option<String> {
    let ty = crate::typechecking::subst::apply_ty_prune(checker.subst(), ty?);
    view_of_resolved(checker, &ty)
}

/// [`view_of`] of a type the substitution is already applied to.
fn view_of_resolved(checker: &Checker, ty: &Ty) -> Option<String> {
    let name = match strip_readonly(ty) {
        Ty::Con(n) => n.clone(),
        Ty::App(head, _) => match head.as_ref() {
            Ty::Con(n) => n.clone(),
            _ => return None,
        },
        _ => return None,
    };
    let key = checker.resolve_class_key(&name).unwrap_or(name);
    checker.is_view_class(&key).then_some(key)
}

/// A view type held inside `ty` (not `ty` itself): a type argument, an
/// array element or a tuple slot.
fn nested_view(checker: &Checker, ty: &Ty) -> Option<String> {
    if !matches!(strip_readonly(ty), Ty::App(..) | Ty::Array { .. } | Ty::Tuple(_)) {
        return None;
    }
    // The substitution once, not again at every level (#2657): `ty` is
    // walked as written, each part's view looked up in its resolved copy.
    nested_view_in(checker, ty, &crate::typechecking::subst::apply_ty_prune(checker.subst(), ty))
}

/// The parts of `ty` as written, with `resolved` (its resolved copy, the
/// same shape where `ty` has one) alongside.
fn nested_view_in(checker: &Checker, ty: &Ty, resolved: &Ty) -> Option<String> {
    let inner = view_parts(ty)?;
    let resolved_inner = view_parts(resolved).filter(|r| r.len() == inner.len());
    inner.into_iter().enumerate().find_map(|(k, t)| match resolved_inner.as_ref().map(|r| r[k]) {
        Some(r) => view_of_resolved(checker, r).or_else(|| nested_view_in(checker, t, r)),
        None => view_of(checker, Some(t)).or_else(|| nested_view(checker, t)),
    })
}

/// The types a view may be held inside of in `ty`: type arguments, an
/// array's element, a tuple's items.
fn view_parts(ty: &Ty) -> Option<Vec<&Ty>> {
    Some(match strip_readonly(ty) {
        Ty::App(_, args) => args.iter().collect(),
        Ty::Array { element, .. } => vec![element.as_ref()],
        Ty::Tuple(items) => items.iter().collect(),
        _ => return None,
    })
}

fn short(key: &str) -> &str {
    key.rsplit("::").next().unwrap_or(key)
}

/// The argument `a`, through `name: value`.
fn named_value(body: &HirBody, a: HirId) -> HirId {
    match &body.expr(a).kind {
        HirKind::Named { value, .. } => *value,
        _ => a,
    }
}

/// The values `new View(…)` node `id` passes to its view class's `mut`
/// fields: the places the view borrows. Empty for anything else.
pub(crate) fn borrow_args(checker: &Checker, body: &HirBody, id: HirId) -> Vec<HirId> {
    let HirKind::Make { kind: MakeKind::Class(class), args } = &body.expr(id).kind else { return Vec::new() };
    let key = checker.resolve_class_key(class).unwrap_or_else(|| class.clone());
    if !checker.is_view_class(&key) {
        return Vec::new();
    }
    let fields = checker.class_fields(&key).unwrap_or_default();
    let mut next = 0usize;
    let mut out = Vec::new();
    for &a in args {
        let (field, value) = match &body.expr(a).kind {
            HirKind::Named { name, value } => (Some(name.clone()), *value),
            _ => {
                let f = fields.get(next).map(|(n, _)| n.clone());
                next += 1;
                (f, a)
            }
        };
        if field.is_some_and(|f| checker.is_borrow_field(&key, &f)) {
            out.push(value);
        }
    }
    out
}

/// Whether `ty` is a view class instance.
pub(crate) fn is_view_ty(checker: &Checker, ty: Option<&Ty>) -> bool {
    view_of(checker, ty).is_some()
}

/// The view class key of `ty`, when it is a view class instance.
pub(crate) fn view_key(checker: &Checker, ty: Option<&Ty>) -> Option<String> {
    view_of(checker, ty)
}

/// Whether `v` is the method's receiver or a place below it: `self`, or a
/// trait method's first parameter (`fn slot(Vec<T> xs, int i)` on
/// `impl Slots for Vec<T>`, called as `v.slot(i)`).
fn rooted_at_self(body: &HirBody, v: HirId) -> bool {
    match &body.expr(v).kind {
        HirKind::Local(l) => body.local(*l).kind == LocalKind::Param && (body.local(*l).name == "self" || body.params.first() == Some(l)),
        HirKind::Field { base, .. } | HirKind::Index { base, .. } => rooted_at_self(body, *base),
        _ => false,
    }
}

/// Whether a method may return `v`: a view it builds over `mut self` (or
/// a place below it), `self` when the method is a view's own, or another
/// view-returning call on `self`.
fn returnable(checker: &Checker, body: &HirBody, v: HirId) -> bool {
    if body.kind != BodyKind::Method {
        return false;
    }
    match &body.expr(v).kind {
        HirKind::Make { kind: MakeKind::Class(_), .. } => borrow_args(checker, body, v).iter().all(|&a| rooted_at_self(body, a)),
        // A local bound once to a returnable view: the result temporary a
        // method with contracts (`ensures`, a class `invariant`) returns.
        HirKind::Local(l) => {
            rooted_at_self(body, v)
                || (body.local(*l).kind != LocalKind::Param && {
                    let mut inits = body.exprs.iter().filter_map(|e| match e.kind {
                        HirKind::Let { local, init: Some(i) } if local == *l => Some(i),
                        HirKind::Assign { place, value } if body.expr(place).kind == HirKind::Local(*l) => Some(value),
                        _ => None,
                    });
                    match (inits.next(), inits.next()) {
                        (Some(i), None) => returnable(checker, body, i),
                        _ => false,
                    }
                })
        }
        HirKind::Call { args, .. } => args.first().is_some_and(|&r| rooted_at_self(body, r)),
        HirKind::Block { tail: Some(t), .. } => returnable(checker, body, *t),
        HirKind::If { then, els: Some(e), .. } => returnable(checker, body, *then) && returnable(checker, body, *e),
        _ => false,
    }
}

/// E0151 errors for `body`.
pub(crate) fn check(checker: &Checker, body: &HirBody) -> Vec<Message> {
    let mut out = Vec::new();
    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    let mut push = |out: &mut Vec<Message>, span: (usize, usize), msg: String, help: &str| {
        if seen.insert(span) {
            let mut m = Message::error(ErrorCode::ViewEscape, msg, span.0..span.1);
            m.with_help(help.to_string());
            out.push(m);
        }
    };
    let held_help = "a view is the borrow it holds: keep it in a local, pass it to a call, or return it from a method of the object it borrows";

    // A lambda that captures a view may run after the borrow ends.
    for &(_, inner) in &body.captures {
        if let Some(view) = view_of(checker, body.local(inner).ty.as_ref()) {
            let name = &body.local(inner).name;
            push(
                &mut out,
                body.span,
                format!("this closure captures the view `{name}` (a `{}`), which may outlive what it borrows", short(&view)),
                "pass the view to the closure as an argument, or call it before the closure",
            );
        }
    }

    for e in &body.exprs {
        let span = e.span;
        if let Some(ty) = &e.ty
            && view_of(checker, Some(ty)).is_none()
            && let Some(view) = nested_view(checker, ty)
            && !matches!(e.kind, HirKind::Lambda { .. })
        {
            push(&mut out, span, format!("a `{}` view cannot be held inside a `{ty}`", short(&view)), held_help);
            continue;
        }
        let ty_view = |v: HirId| view_of(checker, body.expr(v).ty.as_ref());
        match &e.kind {
            HirKind::Assign { place, value } if !matches!(body.expr(*place).kind, HirKind::Local(_)) => {
                if let Some(view) = ty_view(*value) {
                    push(&mut out, span, format!("a `{}` view is stored here, where it would outlive what it borrows", short(&view)), held_help);
                }
            }
            HirKind::Append { value, .. } => {
                if let Some(view) = ty_view(*value) {
                    push(&mut out, span, format!("a `{}` view is stored here, where it would outlive what it borrows", short(&view)), held_help);
                }
            }
            HirKind::Make { kind, args } if !matches!(kind, MakeKind::Range { .. }) => {
                for &a in args {
                    let v = named_value(body, a);
                    if let Some(view) = ty_view(v) {
                        let s = body.expr(v).span;
                        push(&mut out, s, format!("a `{}` view is stored in a new value, where it would outlive what it borrows", short(&view)), held_help);
                    }
                }
            }
            HirKind::Return(Some(v)) => {
                if let Some(view) = ty_view(*v)
                    && !returnable(checker, body, *v)
                {
                    push(
                        &mut out,
                        span,
                        format!("a `{}` view is returned past the place it borrows", short(&view)),
                        "only a method of the borrowed object returns a view: build it over `mut self`, or return `self` from the view's own method",
                    );
                }
            }
            _ => {}
        }
    }
    // A body's tail value is returned too.
    if let Some(root) = body.root
        && let HirKind::Block { tail: Some(t), .. } = &body.expr(root).kind
        && body.ret.is_some()
        && let Some(view) = view_of(checker, body.expr(*t).ty.as_ref())
        && !returnable(checker, body, *t)
    {
        push(
            &mut out,
            body.expr(*t).span,
            format!("a `{}` view is returned past the place it borrows", short(&view)),
            "only a method of the borrowed object returns a view: build it over `mut self`, or return `self` from the view's own method",
        );
    }
    out
}
