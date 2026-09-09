//! In-frame escape facts for ObjEnum / small class values.
//!
//! Fail-closed: a local is frame-local when every *private* use stays in this
//! frame (local match / class field load or store). A named escape (return,
//! call-arg, aggregate, host/FFI) may still unbox if no private use follows
//! it — codegen rematerializes `InitTyped` at that edge (S2j / S2g). Identity
//! compare, method receivers, aliases, nested captures, `fn drop()`, and
//! private use after an escape stay heap. Function parameters stay boxed.

use std::collections::{HashMap, HashSet};

use parser::ast::{EnumConstructPayload, Expression, Output};

use super::id::NodeId;
use super::infer::Checker;
use super::ty::{is_option_ty, is_result_ty, strip_readonly, Ty};

/// Maximum payload arity / field count we will unbox into frame slots.
pub const MAX_UNBOX_SLOTS: usize = 32;

struct Candidate {
    binder: NodeId,
    rhs: NodeId,
    is_class: bool,
}

/// Fill [`Checker::frame_local`] for `ast` after inference.
pub fn analyze_local_escape(checker: &mut Checker, ast: &Output<'_>) {
    checker.frame_local.clear();
    analyze_tree(checker, ast);
}

fn analyze_tree(checker: &mut Checker, ast: &Output<'_>) {
    match ast.1.as_ref() {
        Expression::Program(items) | Expression::Block(items) | Expression::Fragment(items) => {
            analyze_scope(checker, ast);
            for item in items {
                visit_nested_scopes(checker, item);
            }
        }
        Expression::Module(_, body) => analyze_tree(checker, body),
        Expression::Function {
            body: Some(body), ..
        }
        | Expression::TestCase { body, .. }
        | Expression::Lambda { body, .. } => {
            analyze_scope(checker, body);
            visit_nested_scopes(checker, body);
        }
        Expression::Implementation { methods, .. } => {
            for m in methods {
                analyze_tree(checker, m);
            }
        }
        Expression::Method(_, inner) | Expression::Member(inner) => analyze_tree(checker, inner),
        Expression::Statement(inner)
        | Expression::Expr(inner)
        | Expression::ExprStatement(inner)
        | Expression::Group(inner) => analyze_tree(checker, inner),
        _ => {
            analyze_scope(checker, ast);
            visit_nested_scopes(checker, ast);
        }
    }
}

fn visit_nested_scopes(checker: &mut Checker, ast: &Output<'_>) {
    match ast.1.as_ref() {
        Expression::Function {
            body: Some(body), ..
        }
        | Expression::TestCase { body, .. }
        | Expression::Lambda { body, .. } => {
            analyze_scope(checker, body);
            visit_nested_scopes(checker, body);
        }
        Expression::Implementation { methods, .. } => {
            for m in methods {
                visit_nested_scopes(checker, m);
            }
        }
        Expression::Method(_, inner) | Expression::Member(inner) => {
            visit_nested_scopes(checker, inner)
        }
        _ => walk_children(ast, &mut |child| visit_nested_scopes(checker, child)),
    }
}

fn analyze_scope(checker: &mut Checker, ast: &Output<'_>) {
    let mut cands: HashMap<String, Candidate> = HashMap::new();
    collect_candidates(checker, ast, &mut cands);
    if cands.is_empty() && !has_direct_match_construct(ast) {
        mark_direct_match_constructs(checker, ast);
        return;
    }
    let mut escaped: HashSet<String> = HashSet::new();
    let mut saw_escape: HashSet<String> = HashSet::new();
    scan_uses(
        checker,
        ast,
        &cands,
        &mut escaped,
        &mut saw_escape,
        /*nested_fn*/ false,
    );
    let mut used: HashSet<String> = HashSet::new();
    mark_safe_uses(checker, ast, &cands, &escaped, &mut used);
    for (name, cand) in &cands {
        if escaped.contains(name) || !used.contains(name) {
            continue;
        }
        checker.frame_local.insert(cand.binder);
        checker.frame_local.insert(cand.rhs);
    }
    record_last_uses(checker, ast, &cands, &escaped);
    mark_direct_match_constructs(checker, ast);
}

fn collect_candidates(checker: &Checker, ast: &Output<'_>, cands: &mut HashMap<String, Candidate>) {
    match ast.1.as_ref() {
        Expression::Function { .. }
        | Expression::Lambda { .. }
        | Expression::TestCase { .. } => {}
        Expression::Fragment(items) if items.len() == 2 => {
            if let Some(name) = binder_name(&items[0]) {
                let rhs = peel(&items[1]);
                if is_in_frame_ctor(rhs) && candidate_is_unbox(checker, &items[0], &items[1], rhs)
                {
                    let ids = (nid(checker, &items[0]), nid(checker, rhs));
                    if let (Some(binder), Some(rhs_id)) = ids {
                        let is_class = instantiate_is_unbox(checker, rhs)
                            || ty_of(checker, rhs)
                                .or_else(|| ty_of(checker, &items[0]))
                                .is_some_and(|ty| checker.ty_is_class(&ty));
                        cands.insert(
                            name,
                            Candidate {
                                binder,
                                rhs: rhs_id,
                                is_class,
                            },
                        );
                    }
                }
            }
            collect_candidates(checker, &items[1], cands);
        }
        _ => walk_children(ast, &mut |child| collect_candidates(checker, child, cands)),
    }
}

fn scan_uses(
    checker: &Checker,
    ast: &Output<'_>,
    cands: &HashMap<String, Candidate>,
    escaped: &mut HashSet<String>,
    saw_escape: &mut HashSet<String>,
    nested_fn: bool,
) {
    match ast.1.as_ref() {
        Expression::Function {
            body: Some(body), ..
        }
        | Expression::Lambda { body, .. }
        | Expression::TestCase { body, .. } => {
            scan_uses(checker, body, cands, escaped, saw_escape, true);
        }
        Expression::Match { scrutinee, arms } => {
            let s = peel(scrutinee);
            if let Expression::Identifier(n) = s.1.as_ref() {
                if nested_fn && cands.contains_key(*n) {
                    escaped.insert((*n).to_string());
                }
            } else {
                scan_uses(checker, scrutinee, cands, escaped, saw_escape, nested_fn);
            }
            for arm in arms {
                scan_uses(checker, &arm.body, cands, escaped, saw_escape, nested_fn);
            }
        }
        Expression::Access(recv, _) => {
            let r = peel(recv);
            if let Expression::Identifier(n) = r.1.as_ref() {
                if nested_fn && cands.contains_key(*n) {
                    escaped.insert((*n).to_string());
                } else if let Some(cand) = cands.get(*n) {
                    if cand.is_class {
                        note_private_use(*n, escaped, saw_escape);
                    } else {
                        escaped.insert((*n).to_string());
                    }
                }
            } else {
                scan_uses(checker, recv, cands, escaped, saw_escape, nested_fn);
            }
        }
        Expression::OptionalAccess(recv, _) => {
            poison_idents(recv, cands, escaped);
            scan_uses(checker, recv, cands, escaped, saw_escape, nested_fn);
        }
        Expression::Assignment(lhs, rhs) => {
            if let Expression::Access(recv, _) = peel(lhs).1.as_ref() {
                note_field_store(recv, cands, escaped, saw_escape, nested_fn);
                scan_uses(checker, rhs, cands, escaped, saw_escape, nested_fn);
            } else if let Expression::Identifier(n) = peel(lhs).1.as_ref() {
                if cands.contains_key(*n) {
                    let r = peel(rhs);
                    if nested_fn || !is_in_frame_ctor(r) {
                        escaped.insert((*n).to_string());
                    }
                    if let Expression::Identifier(src) = r.1.as_ref() {
                        if cands.contains_key(*src) {
                            escaped.insert((*n).to_string());
                            escaped.insert((*src).to_string());
                        }
                    }
                    scan_uses(checker, rhs, cands, escaped, saw_escape, nested_fn);
                } else {
                    scan_uses(checker, rhs, cands, escaped, saw_escape, nested_fn);
                }
            } else {
                scan_uses(checker, lhs, cands, escaped, saw_escape, nested_fn);
                scan_uses(checker, rhs, cands, escaped, saw_escape, nested_fn);
            }
        }
        Expression::CompoundAssign(lhs, _, rhs) => {
            if let Expression::Access(recv, _) = peel(lhs).1.as_ref() {
                note_field_store(recv, cands, escaped, saw_escape, nested_fn);
            } else {
                poison_idents(lhs, cands, escaped);
            }
            scan_uses(checker, lhs, cands, escaped, saw_escape, nested_fn);
            scan_uses(checker, rhs, cands, escaped, saw_escape, nested_fn);
        }
        Expression::Call { name, args } => {
            let callee = peel(name);
            if let Expression::Access(recv, _) = callee.1.as_ref() {
                poison_idents(recv, cands, escaped);
            } else {
                poison_idents(name, cands, escaped);
            }
            if let Some(args) = args {
                for a in args {
                    mark_named_escape(a, cands, escaped, saw_escape, nested_fn);
                    scan_uses(checker, a, cands, escaped, saw_escape, nested_fn);
                }
            }
            scan_uses(checker, name, cands, escaped, saw_escape, nested_fn);
        }
        Expression::Return(inner)
        | Expression::ImplicitReturn(inner)
        | Expression::Raise(inner)
        | Expression::Yield(inner)
        | Expression::YieldFrom(inner)
        | Expression::Try(inner) => {
            mark_named_escape(inner, cands, escaped, saw_escape, nested_fn);
            scan_uses(checker, inner, cands, escaped, saw_escape, nested_fn);
        }
        Expression::Eq(a, b)
        | Expression::Neq(a, b)
        | Expression::Le(a, b)
        | Expression::Gt(a, b)
        | Expression::Leq(a, b)
        | Expression::Geq(a, b) => {
            for side in [a, b] {
                if let Expression::Identifier(n) = peel(side).1.as_ref() {
                    if cands.contains_key(*n) {
                        escaped.insert((*n).to_string());
                    }
                } else {
                    scan_uses(checker, side, cands, escaped, saw_escape, nested_fn);
                }
            }
        }
        Expression::Identifier(n) => {
            if cands.contains_key(*n) {
                if nested_fn {
                    escaped.insert((*n).to_string());
                } else {
                    saw_escape.insert((*n).to_string());
                }
            }
        }
        Expression::Fragment(items) if items.len() == 2 && binder_name(&items[0]).is_some() => {
            let rhs = peel(&items[1]);
            if let Expression::Identifier(src) = rhs.1.as_ref() {
                if cands.contains_key(*src) {
                    escaped.insert((*src).to_string());
                    if let Some(dst) = binder_name(&items[0]) {
                        escaped.insert(dst);
                    }
                }
            }
            scan_uses(checker, &items[1], cands, escaped, saw_escape, nested_fn);
        }
        Expression::Construct { fields, .. } => match fields {
            EnumConstructPayload::Unit => {}
            EnumConstructPayload::Tuple(args) => {
                for a in args {
                    mark_named_escape(a, cands, escaped, saw_escape, nested_fn);
                    scan_uses(checker, a, cands, escaped, saw_escape, nested_fn);
                }
            }
            EnumConstructPayload::Record(parts) => {
                for p in parts {
                    mark_named_escape(&p.value, cands, escaped, saw_escape, nested_fn);
                    scan_uses(checker, &p.value, cands, escaped, saw_escape, nested_fn);
                }
            }
        },
        Expression::Instantiate(_, Some(args)) => {
            for a in args {
                mark_named_escape(a, cands, escaped, saw_escape, nested_fn);
                scan_uses(checker, a, cands, escaped, saw_escape, nested_fn);
            }
        }
        Expression::Array(items) | Expression::Tuple(items) | Expression::List(items) => {
            for item in items {
                mark_named_escape(item, cands, escaped, saw_escape, nested_fn);
                scan_uses(checker, item, cands, escaped, saw_escape, nested_fn);
            }
        }
        Expression::Index(base, idx) => {
            poison_idents(base, cands, escaped);
            scan_uses(checker, base, cands, escaped, saw_escape, nested_fn);
            if let Some(idx) = idx {
                mark_named_escape(idx, cands, escaped, saw_escape, nested_fn);
                scan_uses(checker, idx, cands, escaped, saw_escape, nested_fn);
            }
        }
        _ => walk_children(ast, &mut |child| {
            scan_uses(checker, child, cands, escaped, saw_escape, nested_fn)
        }),
    }
}

fn mark_safe_uses(
    checker: &mut Checker,
    ast: &Output<'_>,
    cands: &HashMap<String, Candidate>,
    escaped: &HashSet<String>,
    used: &mut HashSet<String>,
) {
    match ast.1.as_ref() {
        Expression::Function { .. } | Expression::Lambda { .. } | Expression::TestCase { .. } => {}
        Expression::Match { scrutinee, arms } => {
            let s = peel(scrutinee);
            if let Expression::Identifier(n) = s.1.as_ref() {
                if cands.contains_key(*n) && !escaped.contains(*n) {
                    used.insert((*n).to_string());
                    if let Some(id) = nid(checker, s).or_else(|| nid(checker, scrutinee)) {
                        checker.frame_local.insert(id);
                    }
                }
            } else {
                mark_safe_uses(checker, scrutinee, cands, escaped, used);
            }
            for arm in arms {
                mark_safe_uses(checker, &arm.body, cands, escaped, used);
            }
        }
        Expression::Access(recv, _) => {
            let r = peel(recv);
            if let Expression::Identifier(n) = r.1.as_ref() {
                if cands.contains_key(*n) && !escaped.contains(*n) {
                    used.insert((*n).to_string());
                    if let Some(id) = nid(checker, r) {
                        checker.frame_local.insert(id);
                    }
                }
            } else {
                mark_safe_uses(checker, recv, cands, escaped, used);
            }
        }
        Expression::Assignment(lhs, rhs) | Expression::CompoundAssign(lhs, _, rhs) => {
            if let Expression::Access(recv, _) = peel(lhs).1.as_ref() {
                let r = peel(recv);
                if let Expression::Identifier(n) = r.1.as_ref() {
                    if cands.contains_key(*n) && !escaped.contains(*n) {
                        used.insert((*n).to_string());
                        if let Some(id) = nid(checker, r) {
                            checker.frame_local.insert(id);
                        }
                    }
                } else {
                    mark_safe_uses(checker, recv, cands, escaped, used);
                }
            }
            mark_safe_uses(checker, rhs, cands, escaped, used);
        }
        _ => walk_children(ast, &mut |child| {
            mark_safe_uses(checker, child, cands, escaped, used)
        }),
    }
}

fn mark_direct_match_constructs(checker: &mut Checker, ast: &Output<'_>) {
    match ast.1.as_ref() {
        Expression::Function {
            body: Some(body), ..
        }
        | Expression::Lambda { body, .. }
        | Expression::TestCase { body, .. } => mark_direct_match_constructs(checker, body),
        Expression::Match { scrutinee, arms } => {
            let s = peel(scrutinee);
            if is_in_frame_ctor(s) && candidate_is_unbox(checker, s, scrutinee, s) {
                if let Some(id) = nid(checker, scrutinee) {
                    checker.frame_local.insert(id);
                }
                if let Some(id) = nid(checker, s) {
                    checker.frame_local.insert(id);
                }
            }
            mark_direct_match_constructs(checker, scrutinee);
            for arm in arms {
                mark_direct_match_constructs(checker, &arm.body);
            }
        }
        _ => walk_children(ast, &mut |child| mark_direct_match_constructs(checker, child)),
    }
}

fn has_direct_match_construct(ast: &Output<'_>) -> bool {
    match ast.1.as_ref() {
        Expression::Match { scrutinee, .. } => is_in_frame_ctor(peel(scrutinee)),
        Expression::Function {
            body: Some(body), ..
        }
        | Expression::Lambda { body, .. }
        | Expression::TestCase { body, .. } => has_direct_match_construct(body),
        _ => {
            let mut found = false;
            walk_children(ast, &mut |child| {
                if has_direct_match_construct(child) {
                    found = true;
                }
            });
            found
        }
    }
}

fn note_private_use(name: &str, escaped: &mut HashSet<String>, saw_escape: &HashSet<String>) {
    if saw_escape.contains(name) {
        escaped.insert(name.to_string());
    }
}

fn note_field_store(
    recv: &Output<'_>,
    cands: &HashMap<String, Candidate>,
    escaped: &mut HashSet<String>,
    saw_escape: &HashSet<String>,
    nested_fn: bool,
) {
    let r = peel(recv);
    if let Expression::Identifier(n) = r.1.as_ref() {
        if cands.contains_key(*n) {
            if nested_fn {
                escaped.insert((*n).to_string());
            } else if cands.get(*n).is_some_and(|c| c.is_class) {
                note_private_use(*n, escaped, saw_escape);
            } else {
                escaped.insert((*n).to_string());
            }
        }
    }
}

/// Whole-object use that can rematerialize at the edge (S2j / S2g).
fn mark_named_escape(
    ast: &Output<'_>,
    cands: &HashMap<String, Candidate>,
    escaped: &mut HashSet<String>,
    saw_escape: &mut HashSet<String>,
    nested_fn: bool,
) {
    match peel(ast).1.as_ref() {
        Expression::Identifier(n) if cands.contains_key(*n) => {
            if nested_fn {
                escaped.insert((*n).to_string());
            } else {
                saw_escape.insert((*n).to_string());
            }
        }
        Expression::Access(recv, _) => {
            let r = peel(recv);
            if let Expression::Identifier(n) = r.1.as_ref() {
                if cands.contains_key(*n) {
                    return;
                }
            }
            walk_children(ast, &mut |child| {
                mark_named_escape(child, cands, escaped, saw_escape, nested_fn)
            });
        }
        _ => walk_children(ast, &mut |child| {
            mark_named_escape(child, cands, escaped, saw_escape, nested_fn)
        }),
    }
}

fn poison_idents(
    ast: &Output<'_>,
    cands: &HashMap<String, Candidate>,
    escaped: &mut HashSet<String>,
) {
    match peel(ast).1.as_ref() {
        Expression::Identifier(n) if cands.contains_key(*n) => {
            escaped.insert((*n).to_string());
        }
        Expression::Access(recv, _) => {
            // Passing `p.x` does not pass `p`.
            let r = peel(recv);
            if let Expression::Identifier(n) = r.1.as_ref() {
                if cands.contains_key(*n) {
                    return;
                }
            }
            walk_children(ast, &mut |child| poison_idents(child, cands, escaped));
        }
        _ => walk_children(ast, &mut |child| poison_idents(child, cands, escaped)),
    }
}

fn is_in_frame_ctor(ast: &Output<'_>) -> bool {
    let peeled = peel(ast);
    match peeled.1.as_ref() {
        Expression::Construct { fields, .. } => !payload_contains_construct(fields),
        Expression::Instantiate(_, args) => {
            args.as_ref().is_none_or(|a| {
                a.iter().all(|e| {
                    !matches!(
                        peel(e).1.as_ref(),
                        Expression::Construct { .. } | Expression::Instantiate(_, _)
                    )
                })
            })
        }
        _ => false,
    }
}

fn payload_contains_construct(fields: &EnumConstructPayload<'_>) -> bool {
    match fields {
        EnumConstructPayload::Unit => false,
        EnumConstructPayload::Tuple(args) => args.iter().any(|a| {
            matches!(
                peel(a).1.as_ref(),
                Expression::Construct { .. } | Expression::Instantiate(_, _)
            )
        }),
        EnumConstructPayload::Record(parts) => parts.iter().any(|p| {
            matches!(
                peel(&p.value).1.as_ref(),
                Expression::Construct { .. } | Expression::Instantiate(_, _)
            )
        }),
    }
}

fn candidate_is_unbox(
    checker: &Checker,
    binder: &Output<'_>,
    unpeeled: &Output<'_>,
    peeled: &Output<'_>,
) -> bool {
    for node in [binder, unpeeled, peeled] {
        if let Some(ty) = ty_of(checker, node) {
            if is_unbox_ty(checker, &ty) {
                return true;
            }
        }
    }
    construct_is_unbox(checker, peeled) || instantiate_is_unbox(checker, peeled)
}

fn construct_is_unbox(checker: &Checker, ast: &Output<'_>) -> bool {
    let Expression::Construct {
        enum_name, fields, ..
    } = peel(ast).1.as_ref()
    else {
        return false;
    };
    let arity = match fields {
        EnumConstructPayload::Unit => 0,
        EnumConstructPayload::Tuple(args) => args.len(),
        EnumConstructPayload::Record(parts) => parts.len(),
    };
    if arity > 1 {
        return false;
    }
    is_unbox_ty(checker, &Ty::Con((*enum_name).to_string()))
}

fn instantiate_is_unbox(checker: &Checker, ast: &Output<'_>) -> bool {
    let Expression::Instantiate(class, _) = peel(ast).1.as_ref() else {
        return false;
    };
    if let Some(ty) = ty_of(checker, ast) {
        if is_unbox_ty(checker, &ty) {
            return true;
        }
    }
    let name = match peel(class).1.as_ref() {
        Expression::Identifier(n) => *n,
        _ => return false,
    };
    is_unbox_ty(checker, &Ty::Con(name.to_string()))
}

fn record_last_uses(
    checker: &mut Checker,
    ast: &Output<'_>,
    cands: &HashMap<String, Candidate>,
    escaped: &HashSet<String>,
) {
    let mut last: HashMap<String, NodeId> = HashMap::new();
    walk_last_ident(checker, ast, cands, escaped, &mut last);
    for (name, id) in last {
        if !escaped.contains(&name) && cands.contains_key(&name) {
            checker.frame_local_last_use.insert(id);
        }
    }
}

fn walk_last_ident(
    checker: &Checker,
    ast: &Output<'_>,
    cands: &HashMap<String, Candidate>,
    escaped: &HashSet<String>,
    last: &mut HashMap<String, NodeId>,
) {
    match ast.1.as_ref() {
        Expression::Function { .. } | Expression::Lambda { .. } | Expression::TestCase { .. } => {}
        Expression::Match { scrutinee, arms } => {
            let s = peel(scrutinee);
            if let Expression::Identifier(n) = s.1.as_ref() {
                if cands.contains_key(*n) && !escaped.contains(*n) {
                    if let Some(id) = nid(checker, s).or_else(|| nid(checker, scrutinee)) {
                        last.insert((*n).to_string(), id);
                    }
                }
            } else {
                walk_last_ident(checker, scrutinee, cands, escaped, last);
            }
            for arm in arms {
                walk_last_ident(checker, &arm.body, cands, escaped, last);
            }
        }
        Expression::Access(recv, _) => {
            let r = peel(recv);
            if let Expression::Identifier(n) = r.1.as_ref() {
                if cands.contains_key(*n) && !escaped.contains(*n) {
                    if let Some(id) = nid(checker, r) {
                        last.insert((*n).to_string(), id);
                    }
                }
            } else {
                walk_last_ident(checker, recv, cands, escaped, last);
            }
        }
        Expression::Identifier(n) => {
            if cands.contains_key(*n) && !escaped.contains(*n) {
                if let Some(id) = nid(checker, ast) {
                    last.insert((*n).to_string(), id);
                }
            }
        }
        _ => walk_children(ast, &mut |child| {
            walk_last_ident(checker, child, cands, escaped, last)
        }),
    }
}

fn is_unbox_ty(checker: &Checker, ty: &Ty) -> bool {
    let ty = strip_readonly(ty);
    if is_option_ty(ty) || is_result_ty(ty) {
        return true;
    }
    if checker.ty_is_class(ty) {
        let Some(name) = Checker::class_name_of_ty(ty) else {
            return false;
        };
        if checker.class_has_drop(name) {
            return false;
        }
        if !matches!(ty, Ty::Con(_)) {
            return false;
        }
        let n = checker.class_fields(name).map(|f| f.len()).unwrap_or(0);
        return n >= 1 && n <= MAX_UNBOX_SLOTS;
    }
    let Some(name) = enum_name(ty) else {
        return false;
    };
    if common::is_builtin_ffi_enum(name) {
        return false;
    }
    if checker.is_scalar_enum(name) {
        return false;
    }
    let Some(vars) = checker.enum_variants(name) else {
        return false;
    };
    let max_arity = vars.iter().map(|(_, _, p)| p.len()).max().unwrap_or(0);
    max_arity <= 1 && !vars.is_empty()
}

fn enum_name(ty: &Ty) -> Option<&str> {
    match strip_readonly(ty) {
        Ty::Con(n) | Ty::Sum { name: n, .. } => Some(n.as_str()),
        Ty::App(head, _) => match head.as_ref() {
            Ty::Con(n) => Some(n.as_str()),
            _ => None,
        },
        Ty::Constructor { owner, .. } => enum_name(owner),
        _ => None,
    }
}

fn ty_of(checker: &Checker, node: &Output<'_>) -> Option<Ty> {
    let id = nid(checker, node)?;
    checker.lookup_at(id).or_else(|| {
        checker.lookup_for_codegen_span(node.0.start, node.0.end)
    })
}

fn nid(checker: &Checker, node: &Output<'_>) -> Option<NodeId> {
    checker.id_table().id_of_output(node)
}

fn binder_name(ast: &Output<'_>) -> Option<String> {
    match peel(ast).1.as_ref() {
        Expression::Variable(n, _) => Some((*n).to_string()),
        Expression::Constant(name, _) => match peel(name).1.as_ref() {
            Expression::Identifier(n) => Some((*n).to_string()),
            _ => None,
        },
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

fn walk_children(ast: &Output<'_>, f: &mut dyn FnMut(&Output<'_>)) {
    match ast.1.as_ref() {
        Expression::Program(items)
        | Expression::Block(items)
        | Expression::Fragment(items)
        | Expression::List(items)
        | Expression::Array(items)
        | Expression::Tuple(items)
        | Expression::If(items) => {
            for item in items {
                f(item);
            }
        }
        Expression::Module(_, inner)
        | Expression::Statement(inner)
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
        | Expression::Yield(inner)
        | Expression::YieldFrom(inner)
        | Expression::Panic(inner)
        | Expression::Access(inner, _)
        | Expression::OptionalAccess(inner, _)
        | Expression::Member(inner)
        | Expression::Method(_, inner)
        | Expression::NamedArg(_, inner)
        | Expression::Defer { body: inner, .. }
        | Expression::Spread(inner) => f(inner),
        Expression::Add(a, b)
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
        | Expression::Le(a, b)
        | Expression::Gt(a, b)
        | Expression::Leq(a, b)
        | Expression::Geq(a, b)
        | Expression::Coalesce(a, b)
        | Expression::Assignment(a, b)
        | Expression::CompoundAssign(a, _, b)
        | Expression::Range {
            start: a, end: b, ..
        } => {
            f(a);
            f(b);
        }
        Expression::Call { name, args } => {
            f(name);
            if let Some(args) = args {
                for a in args {
                    f(a);
                }
            }
        }
        Expression::Match { scrutinee, arms } => {
            f(scrutinee);
            for arm in arms {
                f(&arm.body);
            }
        }
        Expression::Branch(cond, body) => {
            if let Some(c) = cond {
                f(c);
            }
            f(body);
        }
        Expression::Loop {
            identifier,
            iterable,
            body,
        } => {
            if let Some(id) = identifier {
                f(id);
            }
            f(iterable);
            f(body);
        }
        Expression::Construct { fields, .. } => match fields {
            EnumConstructPayload::Unit => {}
            EnumConstructPayload::Tuple(args) => {
                for a in args {
                    f(a);
                }
            }
            EnumConstructPayload::Record(parts) => {
                for p in parts {
                    f(&p.value);
                }
            }
        },
        Expression::Instantiate(class, args) => {
            f(class);
            if let Some(args) = args {
                for a in args {
                    f(a);
                }
            }
        }
        Expression::Index(base, idx) => {
            f(base);
            if let Some(idx) = idx {
                f(idx);
            }
        }
        Expression::LetDestructure { rhs, .. } => f(rhs),
        Expression::Adjust { target, .. } => f(target),
        Expression::Resume(t, arg) => {
            f(t);
            if let Some(a) = arg {
                f(a);
            }
        }
        Expression::Dict(fields) => {
            for field in fields {
                f(&field.value);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use crate::typechecking::infer::Checker;
    use parser::Pratt;

    fn check(src: &str) -> Checker {
        let owned = Box::leak(src.to_string().into_boxed_str());
        let ast = Pratt::default().parse(owned).expect("parse");
        let mut c = Checker::new();
        let _ = c.check_program(&ast);
        assert!(c.messages().is_empty(), "{:?}", c.messages());
        c
    }

    fn frame_local_count(src: &str) -> usize {
        let c = check(src);
        c.typed_sidecar().frame_local_ids().len()
    }

    #[test]
    fn local_option_match_is_frame_local() {
        let src = r#"
fn main() {
    let x = Option::Some(1);
    let y = match x {
        Option::Some(v) => v,
        Option::None => 0,
    };
}
"#;
        assert!(
            frame_local_count(src) >= 2,
            "expected binder + construct + match ident"
        );
    }

    #[test]
    fn field_store_is_frame_local() {
        let src = r#"
class Point {
    pub x: int,
    pub y: int,
}
fn main() {
    let p = new Point(1, 2);
    p.x = 10;
}
"#;
        assert!(
            frame_local_count(src) >= 2,
            "non-escaping field store should unbox"
        );
    }

    #[test]
    fn field_store_then_call_is_frame_local() {
        let src = r#"
class Point {
    pub x: int,
    pub y: int,
}
fn take(Point q) -> int {
    return q.x;
}
fn main() {
    let p = new Point(1, 2);
    p.x = 10;
    let z = take(p);
}
"#;
        assert!(
            frame_local_count(src) >= 2,
            "named escape after private stores still unboxes"
        );
    }

    #[test]
    fn private_use_after_escape_stays_heap() {
        let src = r#"
class Point {
    pub x: int,
    pub y: int,
}
fn take(Point q) -> int {
    return q.x;
}
fn main() {
    let p = new Point(1, 2);
    let z = take(p);
    p.x = 10;
}
"#;
        assert_eq!(
            frame_local_count(src),
            0,
            "private store after call-arg escape must stay heap"
        );
    }


    #[test]
    fn method_receiver_escapes() {
        let src = r#"
class Foo { pub v: int }
impl Foo {
    pub fn bump() -> int { return self.v + 1; }
}
fn main() {
    let f = new Foo(41);
    let y = f.bump();
}
"#;
        assert_eq!(frame_local_count(src), 0, "method receiver must stay heap");
    }

    #[test]
    fn while_field_store_is_frame_local() {
        let src = r#"
class Point {
    pub x: int,
    pub y: int,
}
fn bump(int n) -> int {
    let p = new Point(0, 1);
    let i = 0;
    while i < n {
        p.x = p.x + p.y;
        p.y = p.y + 1;
        i = i + 1;
    }
    return p.x + p.y;
}
fn main() {
    let _ = bump(6);
}
"#;
        assert!(
            frame_local_count(src) >= 2,
            "loop field stores should unbox; count={}",
            frame_local_count(src)
        );
    }

    #[test]
    fn named_class_field_read_is_frame_local() {
        let src = r#"
class Point {
    pub x: int,
    pub y: int,
}
fn main() {
    let p = new Point(5, 6);
    let z = p.x;
}
"#;
        assert!(
            frame_local_count(src) >= 2,
            "non-escaping class local should be frame-local"
        );
    }

    #[test]
    fn option_passed_to_call_escapes() {
        let src = r#"
fn take(Option<int> o) -> int {
    return match o {
        Option::Some(v) => v,
        Option::None => 0,
    };
}
fn main() {
    let x = Option::Some(1);
    let y = take(x);
}
"#;
        let c = check(src);
        let side = c.typed_sidecar();
        // `x` in main escapes; take's parameter is never a candidate.
        let src_ast = {
            let owned = Box::leak(src.to_string().into_boxed_str());
            Pratt::default().parse(owned).unwrap()
        };
        let _ = src_ast;
        assert!(
            side.frame_local_ids().is_empty()
                || side.frame_local_ids().len() < 3,
            "escaping Some should not unbox the call argument; ids={:?}",
            side.frame_local_ids()
        );
    }

    #[test]
    fn option_return_stays_boxed() {
        let src = r#"
fn give() -> Option<int> {
    return Option::Some(1);
}
fn main() {
    let _ = give();
}
"#;
        let n = frame_local_count(src);
        assert_eq!(n, 0, "returned Some must stay boxed");
    }

    #[test]
    fn direct_match_construct_is_frame_local() {
        let src = r#"
fn main() {
    let y = match Option::Some(1) {
        Option::Some(v) => v,
        Option::None => 0,
    };
}
"#;
        assert!(frame_local_count(src) >= 1);
    }
}
