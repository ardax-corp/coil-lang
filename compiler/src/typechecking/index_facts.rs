//! Length / in-bounds sidecar facts for `IndexUnchecked` / `ArrayPin`.
//!
//! Fail-closed: facts are `0 <= i < len(arr)` plus length stability. Yield,
//! `ArrayPush`, host, and length-mutating calls refuse. Callers may transfer
//! a proven `(arr, i)` pair into a helper when every site agrees.

use std::collections::HashSet;

use parser::ast::{AssignOp, EnumConstructPayload, Expression, Output};

use super::id::NodeId;
use super::infer::{Checker, ForInKind};
use super::purity::analyze_pure_fns;
use super::ty::{strip_readonly, vec_element_ty, Ty};

const MAX_FACT_ARRAY_ARITY: usize = 32;

#[derive(Clone, Default)]
struct Env {
    /// Names proven `>= 0` (init from a non-negative const or `++`/`+= k>0`).
    nonneg: HashSet<String>,
    /// `(index, array)` pairs with `index < len(array)` in this region.
    lt_len: HashSet<(String, String)>,
    /// Length of this array (or all arrays, if `*`) is no longer stable.
    len_poison: HashSet<String>,
    all_len_poison: bool,
    yielded: bool,
}

impl Env {
    fn poison_all(&mut self) {
        self.all_len_poison = true;
        self.lt_len.clear();
    }

    fn poison_arr(&mut self, name: &str) {
        self.len_poison.insert(name.to_string());
        self.lt_len.retain(|(_, a)| a != name);
    }

    fn kill_index(&mut self, name: &str) {
        self.lt_len.retain(|(i, _)| i != name);
    }

    fn arr_ok(&self, arr: &str) -> bool {
        !self.all_len_poison && !self.len_poison.contains(arr) && !self.yielded
    }

    fn in_bounds(&self, idx: &str, arr: &str) -> bool {
        self.arr_ok(arr)
            && self.nonneg.contains(idx)
            && self.lt_len.contains(&(idx.to_string(), arr.to_string()))
    }

    /// After a finished loop/if: drop stale `i < len` facts, but do not keep
    /// length poison (a completed fill loop leaves a stable length).
    fn finish_nested_region(&mut self, inner: &Env) {
        if inner.yielded {
            self.yielded = true;
            self.lt_len.clear();
            return;
        }
        if inner.all_len_poison {
            self.lt_len.clear();
            return;
        }
        for a in &inner.len_poison {
            self.lt_len.retain(|(_, arr)| arr != a);
        }
    }
}

struct CallSite {
    callee: String,
    args: Vec<Option<String>>,
    lt_len: HashSet<(String, String)>,
    nonneg: HashSet<String>,
    arr_ok: HashSet<String>,
}

/// Fill length / in-bounds sets on [`Checker`] after inference.
pub fn analyze_index_facts(checker: &mut Checker, ast: &Output<'_>) {
    checker.in_bounds_index.clear();
    checker.pin_array.clear();
    checker.pin_params.clear();
    checker.for_in_pin.clear();
    checker.for_in_pin_spans.clear();

    let pure = analyze_pure_fns(ast);
    let mut calls: Vec<CallSite> = Vec::new();
    let mut used_as_value: HashSet<String> = HashSet::new();
    collect_value_uses(ast, &mut used_as_value);

    let mut shapes: Vec<(String, Vec<(String, NodeId, bool)>)> = Vec::new();
    collect_fn_shapes(checker, ast, &mut shapes);

    walk_tree(checker, ast, &pure, &mut Env::default(), &mut calls);

    apply_interproc(checker, ast, &shapes, &calls, &used_as_value);
    pin_callee_proven_params(checker, ast, &shapes);
}

fn collect_fn_shapes(
    checker: &Checker,
    ast: &Output<'_>,
    out: &mut Vec<(String, Vec<(String, NodeId, bool)>)>,
) {
    match ast.1.as_ref() {
        Expression::Program(items) | Expression::Block(items) | Expression::Fragment(items) => {
            for item in items {
                collect_fn_shapes(checker, item, out);
            }
        }
        Expression::Module(_, body) => collect_fn_shapes(checker, body, out),
        Expression::Function {
            name,
            args,
            body: Some(body),
            ..
        } => {
            out.push(((*name).to_string(), param_list(checker, args)));
            collect_fn_shapes(checker, body, out);
        }
        Expression::Implementation { methods, .. } => {
            for m in methods {
                collect_fn_shapes(checker, m, out);
            }
        }
        Expression::Method(_, inner) | Expression::Member(inner) => {
            collect_fn_shapes(checker, inner, out)
        }
        _ => walk_children(ast, &mut |c| collect_fn_shapes(checker, c, out)),
    }
}

fn param_list(checker: &Checker, args: &Output<'_>) -> Vec<(String, NodeId, bool)> {
    let mut out = Vec::new();
    let kids: &[Output<'_>] = match args.1.as_ref() {
        Expression::Fragment(xs) | Expression::List(xs) => xs,
        Expression::Argument { .. } => {
            if let Some(p) = one_param(checker, args) {
                out.push(p);
            }
            return out;
        }
        _ => return out,
    };
    for a in kids {
        if let Some(p) = one_param(checker, a) {
            out.push(p);
        }
    }
    out
}

fn one_param(checker: &Checker, a: &Output<'_>) -> Option<(String, NodeId, bool)> {
    let Expression::Argument { name, ty, .. } = a.1.as_ref() else {
        return None;
    };
    let id = nid(checker, a)?;
    let arrayish = checker.lookup_at(id).as_ref().is_some_and(is_arrayish)
        || ty.as_ref().is_some_and(ann_is_vec)
        || name_ty(checker, a).is_some_and(|t| is_arrayish(&t));
    Some(((*name).to_string(), id, arrayish))
}

fn ann_is_vec(ty: &Output<'_>) -> bool {
    match peel(ty).1.as_ref() {
        Expression::TypeApp { name, .. } => *name == "Vec" || *name == common::BUILTIN_VEC_TYPE,
        Expression::Type(n) | Expression::Identifier(n) => {
            *n == "Vec" || *n == common::BUILTIN_VEC_TYPE
        }
        _ => false,
    }
}

fn name_ty(checker: &Checker, node: &Output<'_>) -> Option<Ty> {
    nid(checker, node).and_then(|id| checker.lookup_at(id))
}

fn is_arrayish(ty: &Ty) -> bool {
    let ty = strip_readonly(ty);
    vec_element_ty(ty).is_some() || matches!(ty, Ty::Array { .. })
}

fn nid(checker: &Checker, node: &Output<'_>) -> Option<NodeId> {
    checker.id_table().id_of_output(node)
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

fn ident_name(expr: &Output<'_>) -> Option<String> {
    match peel(expr).1.as_ref() {
        Expression::Identifier(n) => Some((*n).to_string()),
        Expression::NamedArg(_, v) => ident_name(v),
        _ => None,
    }
}

/// `len(a)` or `a.len()`.
fn len_of_array(expr: &Output<'_>) -> Option<String> {
    match peel(expr).1.as_ref() {
        Expression::Call { name, args } => match peel(name).1.as_ref() {
            Expression::Identifier(n) if *n == "len" => {
                let args = args.as_ref()?;
                if args.len() == 1 {
                    ident_name(&args[0])
                } else {
                    None
                }
            }
            Expression::Access(recv, field) if *field == "len" => ident_name(recv),
            _ => None,
        },
        Expression::Access(recv, field) if *field == "len" => ident_name(recv),
        _ => None,
    }
}

fn proves_nonneg(expr: &Output<'_>) -> Option<String> {
    match peel(expr).1.as_ref() {
        Expression::Geq(l, r) => match (ident_name(l), peel(r).1.as_ref()) {
            (Some(i), Expression::Integer(n)) if *n >= 0 => Some(i),
            _ => None,
        },
        Expression::Gt(l, r) => match (ident_name(l), peel(r).1.as_ref()) {
            (Some(i), Expression::Integer(n)) if *n >= -1 => Some(i),
            _ => None,
        },
        Expression::Leq(l, r) => match (peel(l).1.as_ref(), ident_name(r)) {
            (Expression::Integer(n), Some(i)) if *n >= 0 => Some(i),
            _ => None,
        },
        Expression::And(l, r) | Expression::BitAnd(l, r) => {
            proves_nonneg(l).or_else(|| proves_nonneg(r))
        }
        _ => None,
    }
}
fn strict_lt_len(expr: &Output<'_>) -> Option<(String, String)> {
    match peel(expr).1.as_ref() {
        Expression::Le(l, r) => {
            let i = ident_name(l)?;
            let a = len_of_array(r)?;
            Some((i, a))
        }
        Expression::Gt(l, r) => {
            let a = len_of_array(l)?;
            let i = ident_name(r)?;
            Some((i, a))
        }
        Expression::And(l, r) | Expression::BitAnd(l, r) => {
            strict_lt_len(l).or_else(|| strict_lt_len(r))
        }
        _ => None,
    }
}

fn is_nonneg_const(expr: &Output<'_>) -> bool {
    matches!(peel(expr).1.as_ref(), Expression::Integer(n) if *n >= 0)
}

fn positive_int_const(expr: &Output<'_>) -> Option<i64> {
    match peel(expr).1.as_ref() {
        Expression::Integer(n) if *n > 0 => Some(*n),
        _ => None,
    }
}

fn is_immediate(expr: &Output<'_>) -> bool {
    matches!(
        peel(expr).1.as_ref(),
        Expression::Integer(_)
            | Expression::Float(_)
            | Expression::Bool(_)
            | Expression::String(_)
    )
}

fn array_lit_refuses_fact(items: &[Output<'_>]) -> bool {
    items.len() > MAX_FACT_ARRAY_ARITY || items.iter().any(|e| !is_immediate(e))
}

fn walk_tree(
    checker: &mut Checker,
    ast: &Output<'_>,
    pure: &HashSet<String>,
    env: &mut Env,
    calls: &mut Vec<CallSite>,
) {
    match ast.1.as_ref() {
        Expression::Program(items)
        | Expression::Block(items)
        | Expression::Fragment(items)
        | Expression::List(items) => {
            for item in items {
                walk_tree(checker, item, pure, env, calls);
            }
        }
        Expression::Module(_, body) => walk_tree(checker, body, pure, env, calls),
        Expression::Function {
            body: Some(body), ..
        }
        | Expression::TestCase { body, .. }
        | Expression::Lambda { body, .. } => {
            let mut inner = Env::default();
            // Params start nonneg only for ints we cannot prove; indices are
            // proven via caller transfer or a local `i < len` header.
            walk_tree(checker, body, pure, &mut inner, calls);
        }
        Expression::Implementation { methods, .. } => {
            for m in methods {
                walk_tree(checker, m, pure, env, calls);
            }
        }
        Expression::Method(_, inner) | Expression::Member(inner) => {
            walk_tree(checker, inner, pure, env, calls)
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
        | Expression::Panic(inner)
        | Expression::Access(inner, _)
        | Expression::OptionalAccess(inner, _)
        | Expression::NamedArg(_, inner)
        | Expression::Spread(inner)
        | Expression::Defer { body: inner, .. } => {
            walk_tree(checker, inner, pure, env, calls);
        }
        Expression::Yield(inner) | Expression::YieldFrom(inner) => {
            env.yielded = true;
            env.lt_len.clear();
            walk_tree(checker, inner, pure, env, calls);
        }
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
        | Expression::Range {
            start: a, end: b, ..
        } => {
            walk_tree(checker, a, pure, env, calls);
            walk_tree(checker, b, pure, env, calls);
        }
        Expression::Variable(name, Some(init)) => {
            walk_tree(checker, init, pure, env, calls);
            bind_name(checker, env, name, init);
        }
        Expression::Constant(name_n, Some(init)) => {
            walk_tree(checker, init, pure, env, calls);
            if let Expression::Identifier(name) = name_n.1.as_ref() {
                bind_name(checker, env, name, init);
            }
        }
        Expression::Assignment(lhs, rhs) => {
            walk_tree(checker, rhs, pure, env, calls);
            walk_tree(checker, lhs, pure, env, calls);
            apply_assign(checker, env, lhs, rhs, None);
        }
        Expression::CompoundAssign(lhs, op, rhs) => {
            walk_tree(checker, rhs, pure, env, calls);
            walk_tree(checker, lhs, pure, env, calls);
            apply_assign(checker, env, lhs, rhs, Some(*op));
        }
        Expression::Adjust { target, .. } => {
            walk_tree(checker, target, pure, env, calls);
            if let Some(n) = ident_name(target) {
                env.nonneg.insert(n.clone());
                env.kill_index(&n);
            }
        }
        Expression::If(branches) => walk_if(checker, branches, pure, env, calls),
        Expression::Branch(cond, body) => {
            if let Some(c) = cond {
                walk_tree(checker, c, pure, env, calls);
                if let Some((i, a)) = strict_lt_len(c) {
                    env.lt_len.insert((i, a));
                }
            }
            walk_tree(checker, body, pure, env, calls);
        }
        Expression::Loop {
            identifier,
            iterable,
            body,
        } => walk_loop(checker, ast, identifier.as_ref(), iterable, body, pure, env, calls),
        Expression::Call { name, args } => {
            walk_tree(checker, name, pure, env, calls);
            if let Some(args) = args {
                for a in args {
                    walk_tree(checker, a, pure, env, calls);
                }
            }
            note_call(env, calls, name, args.as_deref(), pure);
        }
        Expression::Index(base, Some(idx)) => {
            walk_tree(checker, base, pure, env, calls);
            walk_tree(checker, idx, pure, env, calls);
            if let (Some(arr), Some(i)) = (ident_name(base), ident_name(idx))
                && env.in_bounds(&i, &arr)
                && let Some(id) = nid(checker, ast)
            {
                checker.in_bounds_index.insert(id);
            }
        }
        Expression::Index(base, None) => walk_tree(checker, base, pure, env, calls),
        Expression::Array(items) => {
            for it in items {
                walk_tree(checker, it, pure, env, calls);
            }
        }
        Expression::Tuple(items) => {
            for it in items {
                walk_tree(checker, it, pure, env, calls);
            }
        }
        Expression::Match { scrutinee, arms } => {
            walk_tree(checker, scrutinee, pure, env, calls);
            for arm in arms {
                walk_tree(checker, &arm.body, pure, env, calls);
            }
        }
        Expression::Construct { fields, .. } => match fields {
            EnumConstructPayload::Unit => {}
            EnumConstructPayload::Tuple(args) => {
                for a in args {
                    walk_tree(checker, a, pure, env, calls);
                }
            }
            EnumConstructPayload::Record(parts) => {
                for p in parts {
                    walk_tree(checker, &p.value, pure, env, calls);
                }
            }
        },
        Expression::Instantiate(class, args) => {
            walk_tree(checker, class, pure, env, calls);
            if let Some(args) = args {
                for a in args {
                    walk_tree(checker, a, pure, env, calls);
                }
            }
        }
        Expression::LetDestructure { rhs, .. } => walk_tree(checker, rhs, pure, env, calls),
        Expression::Resume(t, arg) => {
            walk_tree(checker, t, pure, env, calls);
            if let Some(a) = arg {
                walk_tree(checker, a, pure, env, calls);
            }
            env.poison_all();
        }
        Expression::Dict(fields) => {
            for f in fields {
                walk_tree(checker, &f.value, pure, env, calls);
            }
        }
        _ => walk_children(ast, &mut |c| walk_tree(checker, c, pure, env, calls)),
    }
}

fn bind_name(checker: &Checker, env: &mut Env, name: &str, init: &Output<'_>) {
    env.kill_index(name);
    if is_nonneg_const(init) {
        env.nonneg.insert(name.to_string());
    } else if let Some(src) = ident_name(init) {
        if env.nonneg.contains(&src) {
            env.nonneg.insert(name.to_string());
        } else {
            env.nonneg.remove(name);
        }
    } else {
        env.nonneg.remove(name);
    }
    if let Expression::Array(items) = peel(init).1.as_ref()
        && array_lit_refuses_fact(items)
    {
        env.poison_arr(name);
    }
    let _ = checker;
}

fn apply_assign(
    checker: &Checker,
    env: &mut Env,
    lhs: &Output<'_>,
    rhs: &Output<'_>,
    compound: Option<AssignOp>,
) {
    match peel(lhs).1.as_ref() {
        Expression::Identifier(n) => {
            if matches!(compound, Some(AssignOp::Add)) && positive_int_const(rhs).is_some() {
                env.nonneg.insert((*n).to_string());
                env.kill_index(n);
                return;
            }
            if compound.is_none() {
                if let Expression::Add(l, r) = peel(rhs).1.as_ref()
                    && ident_name(l).as_deref() == Some(*n)
                    && positive_int_const(r).is_some()
                {
                    env.nonneg.insert((*n).to_string());
                    env.kill_index(n);
                    return;
                }
                bind_name(checker, env, n, rhs);
            } else {
                env.kill_index(n);
                env.nonneg.remove(*n);
            }
        }
        Expression::Index(base, _) => {
            // Element store does not change length.
            let _ = base;
        }
        Expression::Access(_, _) => env.poison_all(),
        _ => {}
    }
}

fn walk_if(
    checker: &mut Checker,
    branches: &[Output<'_>],
    pure: &HashSet<String>,
    env: &mut Env,
    calls: &mut Vec<CallSite>,
) {
    if branches.is_empty() {
        return;
    }
    let start = env.clone();
    for br in branches {
        let (cond, body) = match br.1.as_ref() {
            Expression::Branch(c, b) => (c.as_ref(), b),
            _ => {
                walk_tree(checker, br, pure, env, calls);
                continue;
            }
        };
        let mut branch_env = start.clone();
        if let Some(c) = cond {
            walk_tree(checker, c, pure, &mut branch_env, calls);
            if let Some(i) = proves_nonneg(c) {
                branch_env.nonneg.insert(i);
            }
            if let Some((i, a)) = strict_lt_len(c)
                && branch_env.nonneg.contains(&i)
                && branch_env.arr_ok(&a)
            {
                branch_env.lt_len.insert((i, a));
            }
        }
        walk_tree(checker, body, pure, &mut branch_env, calls);
        env.finish_nested_region(&branch_env);
        env.nonneg = env.nonneg.intersection(&branch_env.nonneg).cloned().collect();
    }
}

fn walk_loop(
    checker: &mut Checker,
    loop_node: &Output<'_>,
    identifier: Option<&Output<'_>>,
    iterable: &Output<'_>,
    body: &Output<'_>,
    pure: &HashSet<String>,
    env: &mut Env,
    calls: &mut Vec<CallSite>,
) {
    if let Some(binding) = identifier {
        // for-in
        walk_tree(checker, iterable, pure, env, calls);
        let mut body_env = env.clone();
        let arr_name = ident_name(iterable);
        let kind_array = nid(checker, loop_node)
            .and_then(|id| checker.for_in_info_at(id))
            .is_some_and(|i| matches!(i.kind, ForInKind::Array))
            || checker
                .for_in_info_span(loop_node.0.start, loop_node.0.end)
                .is_some_and(|i| matches!(i.kind, ForInKind::Array))
            || nid(checker, iterable)
                .and_then(|id| checker.lookup_at(id))
                .as_ref()
                .is_some_and(is_arrayish);
        walk_tree(checker, body, pure, &mut body_env, calls);
        let stable = !body_env.yielded
            && !body_env.all_len_poison
            && arr_name
                .as_ref()
                .is_none_or(|a| !body_env.len_poison.contains(a));
        if kind_array && stable && !env.yielded {
            if let Some(id) = nid(checker, loop_node) {
                checker.for_in_pin.insert(id);
            }
            checker
                .for_in_pin_spans
                .insert((loop_node.0.start, loop_node.0.end));
        }
        env.finish_nested_region(&body_env);
        let _ = binding;
        return;
    }

    // while cond { body }
    walk_tree(checker, iterable, pure, env, calls);
    let mut body_env = env.clone();
    let header = strict_lt_len(iterable);
    if let Some((i, a)) = header.clone() {
        body_env.nonneg.insert(i.clone());
        if body_env.arr_ok(&a) {
            body_env.lt_len.insert((i, a));
        }
    }
    walk_tree(checker, body, pure, &mut body_env, calls);
    env.finish_nested_region(&body_env);
    if let Some((i, _)) = header {
        env.kill_index(&i);
    }
}

fn note_call(
    env: &mut Env,
    calls: &mut Vec<CallSite>,
    name: &Output<'_>,
    args: Option<&[Output<'_>]>,
    pure: &HashSet<String>,
) {
    let callee = match peel(name).1.as_ref() {
        Expression::Identifier(n) => Some((*n).to_string()),
        Expression::Access(recv, field) => {
            if *field == "push" {
                if let Some(arr) = ident_name(recv) {
                    env.poison_arr(&arr);
                } else {
                    env.poison_all();
                }
            } else if *field != "len" {
                env.poison_all();
            }
            None
        }
        Expression::QualifiedAccess { member, .. }
            if matches!(*member, "len" | "new" | "with_capacity") =>
        {
            None
        }
        _ => {
            env.poison_all();
            None
        }
    };
    let Some(callee) = callee else {
        return;
    };
    if callee == "len" {
        return;
    }
    let arg_names: Vec<Option<String>> = args
        .unwrap_or(&[])
        .iter()
        .map(ident_name)
        .collect();
    let mut arr_ok = HashSet::new();
    for a in arg_names.iter().flatten() {
        if env.arr_ok(a) {
            arr_ok.insert(a.clone());
        }
    }
    calls.push(CallSite {
        callee: callee.clone(),
        args: arg_names,
        lt_len: env.lt_len.clone(),
        nonneg: env.nonneg.clone(),
        arr_ok,
    });
    if !pure.contains(&callee) {
        env.poison_all();
    }
}

fn collect_value_uses(ast: &Output<'_>, used: &mut HashSet<String>) {
    match ast.1.as_ref() {
        Expression::Call { name, args } => {
            // Callee position is not a value use of the function ident.
            if !matches!(peel(name).1.as_ref(), Expression::Identifier(_)) {
                collect_value_uses(name, used);
            } else {
                walk_children(name, &mut |c| collect_value_uses(c, used));
            }
            if let Some(args) = args {
                for a in args {
                    collect_value_uses(a, used);
                }
            }
        }
        Expression::Identifier(n) => {
            used.insert((*n).to_string());
        }
        _ => walk_children(ast, &mut |c| collect_value_uses(c, used)),
    }
}

fn apply_interproc(
    checker: &mut Checker,
    ast: &Output<'_>,
    shapes: &[(String, Vec<(String, NodeId, bool)>)],
    calls: &[CallSite],
    used_as_value: &HashSet<String>,
) {
    for (fname, params) in shapes {
        if used_as_value.contains(fname) {
            continue;
        }
        let Some(body) = fn_body(ast, fname) else {
            continue;
        };
        if contains_yield(body) {
            continue;
        }
        let sites: Vec<&CallSite> = calls.iter().filter(|c| c.callee == *fname).collect();
        if sites.is_empty() {
            continue;
        }
        let arrs: Vec<(usize, String, NodeId)> = params
            .iter()
            .enumerate()
            .filter_map(|(i, (n, id, is_a))| is_a.then_some((i, n.clone(), *id)))
            .collect();
        let idxs: Vec<(usize, String)> = params
            .iter()
            .enumerate()
            .filter(|(_, (_, _, is_a))| !*is_a)
            .map(|(i, (n, _, _))| (i, n.clone()))
            .collect();

        for (ai, aname, aid) in &arrs {
            let mut pin = false;
            for (ii, iname) in &idxs {
                let proven = sites.iter().all(|s| {
                    let Some(aarg) = s.args.get(*ai).and_then(|x| x.as_ref()) else {
                        return false;
                    };
                    let Some(iarg) = s.args.get(*ii).and_then(|x| x.as_ref()) else {
                        return false;
                    };
                    s.arr_ok.contains(aarg)
                        && s.nonneg.contains(iarg)
                        && s.lt_len.contains(&(iarg.clone(), aarg.clone()))
                });
                if !proven {
                    continue;
                }
                mark_callee_indices(checker, body, aname, iname);
                pin = true;
            }
            if pin {
                checker.pin_array.insert(*aid);
                checker.pin_params.insert((fname.clone(), aname.clone()));
            }
        }
    }
}

/// Pin array params that the callee itself proves (`i < a.len()` in-body).
fn pin_callee_proven_params(
    checker: &mut Checker,
    ast: &Output<'_>,
    shapes: &[(String, Vec<(String, NodeId, bool)>)],
) {
    for (fname, params) in shapes {
        let Some(body) = fn_body(ast, fname) else {
            continue;
        };
        if contains_yield(body) {
            continue;
        }
        for (aname, aid, is_a) in params {
            if !*is_a {
                continue;
            }
            if body_has_in_bounds_index_of(checker, body, aname) {
                checker.pin_array.insert(*aid);
                checker.pin_params.insert((fname.clone(), aname.clone()));
            }
        }
    }
}

fn body_has_in_bounds_index_of(checker: &Checker, body: &Output<'_>, arr: &str) -> bool {
    match body.1.as_ref() {
        Expression::Index(base, Some(_)) => {
            ident_name(base).as_deref() == Some(arr)
                && nid(checker, body).is_some_and(|id| checker.in_bounds_index.contains(&id))
                || {
                    let mut hit = false;
                    walk_children(body, &mut |c| {
                        if body_has_in_bounds_index_of(checker, c, arr) {
                            hit = true;
                        }
                    });
                    hit
                }
        }
        Expression::Yield(_) | Expression::YieldFrom(_) => false,
        _ => {
            let mut hit = false;
            walk_children(body, &mut |c| {
                if body_has_in_bounds_index_of(checker, c, arr) {
                    hit = true;
                }
            });
            hit
        }
    }
}

fn fn_body<'a>(ast: &'a Output<'a>, name: &str) -> Option<&'a Output<'a>> {
    match ast.1.as_ref() {
        Expression::Program(items) | Expression::Block(items) | Expression::Fragment(items) => {
            items.iter().find_map(|i| fn_body(i, name))
        }
        Expression::Module(_, body) => fn_body(body, name),
        Expression::Function {
            name: n,
            body: Some(body),
            ..
        } if *n == name => Some(body),
        Expression::Function {
            body: Some(body), ..
        } => fn_body(body, name),
        Expression::Implementation { methods, .. } => methods.iter().find_map(|m| fn_body(m, name)),
        Expression::Method(_, inner) | Expression::Member(inner) => fn_body(inner, name),
        _ => None,
    }
}

fn contains_yield(ast: &Output<'_>) -> bool {
    match ast.1.as_ref() {
        Expression::Yield(_) | Expression::YieldFrom(_) => true,
        _ => {
            let mut hit = false;
            walk_children(ast, &mut |c| {
                if contains_yield(c) {
                    hit = true;
                }
            });
            hit
        }
    }
}

fn mark_callee_indices(checker: &mut Checker, body: &Output<'_>, arr: &str, idx: &str) {
    match body.1.as_ref() {
        Expression::Index(base, Some(i)) => {
            if ident_name(base).as_deref() == Some(arr) && ident_name(i).as_deref() == Some(idx)
                && let Some(id) = nid(checker, body)
            {
                checker.in_bounds_index.insert(id);
            }
            walk_children(body, &mut |c| mark_callee_indices(checker, c, arr, idx));
        }
        Expression::Yield(_) | Expression::YieldFrom(_) => {}
        _ => walk_children(body, &mut |c| mark_callee_indices(checker, c, arr, idx)),
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
        Expression::Index(base, idx) => {
            f(base);
            if let Some(idx) = idx {
                f(idx);
            }
        }
        Expression::Function {
            args,
            body: Some(body),
            ..
        } => {
            f(args);
            f(body);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use parser::Pratt;

    use crate::typechecking::infer::Checker;

    fn sidecar(src: &str) -> crate::typechecking::TypedSidecar {
        let owned = Box::leak(src.to_string().into_boxed_str());
        let ast = Pratt::default().parse(owned).expect("parse");
        let mut c = Checker::new();
        let _ = c.check_program(&ast);
        c.typed_sidecar()
    }

    #[test]
    fn counted_loop_marks_index() {
        let src = r#"
fn main() -> int {
    let b: Vec<int> = Vec::new();
    let n = 4;
    let i = 0;
    while i < n {
        b.push(i);
        i = i + 1;
    }
    let acc = 0;
    let j = 0;
    while j < len(b) {
        acc = acc + b[j];
        j = j + 1;
    }
    return acc;
}
"#;
        let s = sidecar(src);
        assert!(
            !s.in_bounds_index_ids().is_empty(),
            "b[j] under j < len(b) should be in-bounds"
        );
    }

    #[test]
    fn yield_loop_refuses_index_fact() {
        let src = r#"
async fn scan(Vec<int> b) {
    let j = 0;
    while j < len(b) {
        yield b[j];
        j = j + 1;
    }
}
fn main() {
    let b: Vec<int> = Vec::new();
    b.push(1);
    let h = scan(b);
    resume h;
}
"#;
        let s = sidecar(src);
        assert!(
            s.in_bounds_index_ids().is_empty(),
            "yield must refuse in-bounds facts"
        );
    }

    #[test]
    fn helper_caller_records_pin_param() {
        let src = r#"
fn at(Vec<int> a, int i) -> int {
    let t = 0;
    let k = 0;
    while k < 4 {
        t = t + 1;
        k = k + 1;
    }
    return a[i] + t - 4;
}
fn main() -> int {
    let b: Vec<int> = Vec::new();
    let n = 8;
    let i = 0;
    while i < n {
        b.push(i);
        i = i + 1;
    }
    let acc = 0;
    let j = 0;
    while j < len(b) {
        acc = acc + at(b, j);
        j = j + 1;
    }
    return acc;
}
"#;
        let s = sidecar(src);
        assert!(
            !s.in_bounds_index_ids().is_empty(),
            "caller-proven a[i] should be in-bounds"
        );
        assert!(
            s.is_pin_param("at", "a"),
            "helper param a should be pin_params"
        );
    }
}
