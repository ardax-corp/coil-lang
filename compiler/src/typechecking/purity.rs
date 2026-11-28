//! The effect walk over the AST, for the two questions asked before there
//! is any HIR:
//!
//! - which functions can never change an array's length
//!   ([`record_length_stability`]), for the checker's bounds proofs
//!   (`index_facts`);
//! - whether a `fn drop()` anywhere in the compile may
//!   ([`finalizer_may_resize`]), for the pipeline.
//!
//! Everything else that asks about effects (purity, auto-par, `uses {…}`,
//! copy-on-write, escape) reads the HIR summaries (`hir::effects`).
//!
//! The walk still tracks every effect bit, not only `RESIZE`: when a `drop`
//! may resize, a finalizer can run at any allocation, and only a pure call
//! is length-stable then.

use std::collections::{HashMap, HashSet};

use parser::ast::{EnumConstructPayload, Expression, LetPattern, Output, PatternOutput};

use crate::effects::{LengthStability, classify_host_name};

use super::bare_cases::BareCases;
use super::id::walk_children;

use common::EffectFlags;

#[derive(Default)]
struct FnFacts {
    local: EffectFlags,
    /// Callee names (unqualified Identifier call targets).
    callees: HashSet<String>,
    /// Names bound in the body (params, `let`, patterns). A call through one
    /// of these is a function value, not the host or user `fn` it shadows.
    bound: HashSet<String>,
    /// Locals whose object never leaves the function (see [`private_locals`]).
    /// A write through one is not `HEAP_MUT`: no caller can see it.
    private: HashSet<String>,
    /// `recv.m(..)` calls as (receiver span, method name). The receiver's
    /// type picks the impl in [`settle_method_calls`]; until then they cost
    /// nothing.
    method_calls: Vec<((usize, usize), String)>,
    /// Names read / assigned that the body does not bind: statics among
    /// them are settled by [`settle_statics`].
    free: HashSet<String>,
    free_writes: HashSet<String>,
    /// Operand spans of the operators a trait impl may stand behind
    /// (`a < b`, `-a`, `a += b`), and the spans of `for … in` loops. With
    /// the checker's types, [`settle_operators`] turns the ones that run
    /// user code into effects; a lone-file walk has no types and leaves
    /// them pure.
    operands: Vec<(usize, usize)>,
    for_ins: Vec<(usize, usize)>,
    /// `Owner::name(..)` and `Owner::name`, written like an enum
    /// constructor (the flag is the form without arguments). The checker
    /// says which are one; [`settle_constructs`] makes the rest a call of
    /// `Owner::name` (a static method, a trait's static method, a module's
    /// function) or a class static. A lone-file walk leaves them pure.
    constructs: Vec<(String, String, bool)>,
}

/// Record `f` under `name`. Methods are keyed by bare name, so two impls with
/// the same method name share one entry: union them rather than letting the
/// last body win.
fn insert_facts(facts: &mut HashMap<String, FnFacts>, name: &str, f: FnFacts) {
    let entry = facts.entry(name.to_string()).or_default();
    entry.local = entry.local.union(f.local);
    entry.callees.extend(f.callees);
    entry.bound.extend(f.bound);
    entry.method_calls.extend(f.method_calls);
    entry.free.extend(f.free);
    entry.free_writes.extend(f.free_writes);
    entry.operands.extend(f.operands);
    entry.for_ins.extend(f.for_ins);
    entry.constructs.extend(f.constructs);
}

/// `static let` names declared anywhere in `ast`.
fn static_names(ast: &Output<'_>, out: &mut HashSet<String>) {
    if let Expression::StaticDecl { name, is_const: false, .. } = ast.1.as_ref() {
        out.insert((*name).to_string());
    }
    walk_children(ast, &mut |c| static_names(c, out));
}

/// A static is shared mutable state: reading one is `HOST` (another call
/// may have changed it, and auto-par workers have no statics), writing one
/// is `HEAP_MUT` (coil-lang#793).
fn settle_statics(facts: &mut HashMap<String, FnFacts>, ast: &Output<'_>) {
    let mut statics = HashSet::new();
    static_names(ast, &mut statics);
    if statics.is_empty() {
        return;
    }
    for f in facts.values_mut() {
        if f.free.iter().any(|n| statics.contains(n)) {
            f.local.insert(EffectFlags::HOST);
        }
        if f.free_writes.iter().any(|n| statics.contains(n)) {
            f.local.insert(EffectFlags::HEAP_MUT);
        }
    }
}

/// Receiver span and method name to the `Owner::m` callee key.
type MethodTarget<'a> = dyn Fn((usize, usize), &str) -> Option<String> + 'a;

/// Turn each fn's `recv.m(..)` calls into callees. `target` names the impl
/// method (`Owner::m`) for a receiver span when its type is a user class;
/// anything else stays unknown, except `len` / `capacity`, which cannot resize.
fn settle_method_calls(facts: &mut HashMap<String, FnFacts>, target: &MethodTarget<'_>) {
    for f in facts.values_mut() {
        for (span, m) in std::mem::take(&mut f.method_calls) {
            if let Some(key) = target(span, &m) {
                f.callees.insert(key);
            } else if matches!(m.as_str(), "len" | "capacity") {
                f.local.insert(EffectFlags::UNKNOWN);
            } else {
                f.local.insert(EffectFlags::UNKNOWN | EffectFlags::RESIZE);
            }
        }
    }
}

/// An operator on a class, an enum or a type parameter runs the type's
/// trait impl, and a `for … in` over anything but an array, a tuple or a
/// range runs an iterator's `next` or resumes a generator: user code this
/// walk does not follow, so unknown, and free to resize (#2388).
/// `operand_runs` and `for_in_runs` answer for an operand's and a loop's
/// span.
fn settle_operators(
    facts: &mut HashMap<String, FnFacts>,
    operand_runs: &dyn Fn((usize, usize)) -> bool,
    for_in_runs: &dyn Fn((usize, usize)) -> bool,
) {
    for f in facts.values_mut() {
        let operands = std::mem::take(&mut f.operands);
        let for_ins = std::mem::take(&mut f.for_ins);
        if operands.into_iter().any(operand_runs) || for_ins.into_iter().any(for_in_runs) {
            f.local.insert(EffectFlags::UNKNOWN | EffectFlags::RESIZE);
        }
    }
}

/// `Owner::name(..)` that is not an enum constructor is a call of
/// `Owner::name`, and so are `Owner::name()` and the bare `Owner::name`
/// (its value, #2746) when `name` is a static method (`is_call`). Any other
/// bare `Owner::name` that is no constructor reads or writes a class static
/// (#2407). `is_variant` answers for an owner and a name.
fn settle_constructs(facts: &mut HashMap<String, FnFacts>, is_variant: &dyn Fn(&str, &str) -> bool, is_call: &dyn Fn(&str, &str) -> bool) {
    for f in facts.values_mut() {
        for (owner, name, bare) in std::mem::take(&mut f.constructs) {
            if is_variant(&owner, &name) {
                continue;
            }
            if bare && !is_call(&owner, &name) {
                f.local.insert(EffectFlags::HOST | EffectFlags::HEAP_MUT);
            } else {
                f.callees.insert(format!("{owner}::{name}"));
            }
        }
    }
}

/// Every name `ast` binds, at any depth: params, `let`, `const`, for-in and
/// match / `if let` pattern bindings.
pub(super) fn collect_binders(ast: &Output<'_>, cases: &BareCases, out: &mut HashSet<String>) {
    each_binder(ast, cases, &mut |n| {
        out.insert(n.to_string());
    });
}

/// Call `f` once per binding site in `ast` (see [`collect_binders`]).
fn each_binder(ast: &Output<'_>, cases: &BareCases, f: &mut dyn FnMut(&str)) {
    let mut here: HashSet<String> = HashSet::new();
    let out = &mut here;
    match ast.1.as_ref() {
        Expression::Variable(n, _) | Expression::Argument { name: n, .. } => {
            out.insert((*n).to_string());
        }
        Expression::Constant(name, ..) => {
            if let Expression::Identifier(n) = peel(name).1.as_ref() {
                out.insert((*n).to_string());
            }
        }
        Expression::LetDestructure { pattern, .. } => let_pattern_binders(pattern, out),
        Expression::Loop {
            identifier,
            pattern,
            ..
        } => {
            if let Some(id) = identifier
                && let Expression::Identifier(n) = peel(id).1.as_ref()
            {
                out.insert((*n).to_string());
            }
            if let Some(p) = pattern {
                let_pattern_binders(p, out);
            }
        }
        Expression::Match { arms, .. } => {
            for arm in arms {
                pattern_binders(&arm.pattern, cases, out);
            }
        }
        Expression::IfLet {
            then_arm, else_arm, ..
        } => {
            pattern_binders(&then_arm.pattern, cases, out);
            pattern_binders(&else_arm.pattern, cases, out);
        }
        Expression::WhileLet {
            then_arm, on_miss, ..
        } => {
            pattern_binders(&then_arm.pattern, cases, out);
            pattern_binders(&on_miss.pattern, cases, out);
        }
        _ => {}
    }
    for n in &here {
        f(n);
    }
    walk_children(ast, &mut |c| each_binder(c, cases, f));
}

fn let_pattern_binders(p: &LetPattern<'_>, out: &mut HashSet<String>) {
    match p {
        LetPattern::Wildcard => {}
        LetPattern::Binding { name } => {
            out.insert((*name).to_string());
        }
        LetPattern::Tuple(items) => {
            for item in items {
                let_pattern_binders(item, out);
            }
        }
        LetPattern::Record(fields) => {
            for f in fields {
                let_pattern_binders(&f.pattern, out);
            }
        }
    }
}

/// Names a match / `if let` pattern binds; a bare unit case (`None`, `Red`)
/// binds nothing (#1394).
fn pattern_binders(p: &PatternOutput<'_>, cases: &BareCases, out: &mut HashSet<String>) {
    cases.each_binder(p, &mut |name, _| {
        out.insert(name.to_string());
    });
}

/// Facts for one `fn` declaration: its body effects plus every bound name.
fn fn_facts(args: &Output<'_>, body: &Output<'_>, cases: &BareCases) -> FnFacts {
    let mut f = FnFacts {
        private: private_locals(args, body, cases),
        ..FnFacts::default()
    };
    walk_body(body, &mut f);
    collect_binders(args, cases, &mut f.bound);
    collect_binders(body, cases, &mut f.bound);
    let bound = &f.bound;
    f.free.retain(|n| !bound.contains(n));
    f.free_writes.retain(|n| !bound.contains(n));
    f
}

/// Collect per-function callee sets (and local impurity) for call-graph analyses.
fn collect_fn_facts(ast: &Output<'_>, cases: &BareCases) -> HashMap<String, FnFacts> {
    let mut facts: HashMap<String, FnFacts> = HashMap::new();
    collect_fns(ast, &mut facts, false, cases);
    settle_method_calls(&mut facts, &|_, _| None);
    settle_statics(&mut facts, ast);
    facts
}

/// Per-function effect flags after call-graph closure (unknown → impure).
pub fn analyze_fn_effects(ast: &Output<'_>, cases: &BareCases) -> HashMap<String, EffectFlags> {
    let facts = collect_fn_facts(ast, cases);
    effect_closure(&facts)
}

/// Whether a `fn drop()` declared in `ast` may change an array's length.
///
/// This runs before the checker, so every bare pattern name counts as a
/// binding. Only a function or static that shares a unit case's name and is
/// used in the same body could tell the difference.
pub fn finalizer_may_resize(ast: &Output<'_>) -> bool {
    finalizers_resize(&analyze_fn_effects(ast, BareCases::none()))
}

fn finalizers_resize(effects: &HashMap<String, EffectFlags>) -> bool {
    effects
        .get("drop")
        .is_some_and(|f| f.contains(EffectFlags::RESIZE))
}

/// Length-stable user functions for this file. `program_finalizers_resize`
/// is the whole-compile answer from the pipeline when it has one; a drop in
/// this file counts either way.
pub fn length_stability(
    effects: &HashMap<String, EffectFlags>,
    program_finalizers_resize: bool,
) -> LengthStability {
    let alloc_stable = !program_finalizers_resize && !finalizers_resize(effects);
    let fns = effects
        .iter()
        .filter(|(_, f)| {
            if alloc_stable {
                !f.contains(EffectFlags::RESIZE)
            } else {
                f.is_pure()
            }
        })
        .map(|(name, _)| name.clone())
        .collect();
    LengthStability { fns, alloc_stable }
}

fn effect_closure(facts: &HashMap<String, FnFacts>) -> HashMap<String, EffectFlags> {
    effect_closure_with(facts, &|_| None)
}

/// [`effect_closure`] where `resolve` gives the effects of a callee that is
/// not a `fn` in this file (an imported user function, already analysed when
/// its own module was checked). `None` falls back to the host table.
fn effect_closure_with(
    facts: &HashMap<String, FnFacts>,
    resolve: &dyn Fn(&str) -> Option<EffectFlags>,
) -> HashMap<String, EffectFlags> {
    let user_fns: HashSet<&String> = facts.keys().collect();
    let mut out: HashMap<String, EffectFlags> = HashMap::new();
    for (name, f) in facts {
        let mut flags = f.local;
        for c in &f.callees {
            if f.bound.contains(c) {
                // A local function value: anything could be behind it.
                flags = flags.union(EffectFlags::from_bits(
                    EffectFlags::UNKNOWN | EffectFlags::HOST | EffectFlags::RESIZE,
                ));
            } else if !user_fns.contains(c) {
                flags = flags.union(resolve(c).unwrap_or_else(|| classify_unknown_callee(c)));
            }
        }
        out.insert(name.clone(), flags);
    }
    let mut changed = true;
    while changed {
        changed = false;
        for (name, f) in facts {
            let mut flags = out[name];
            for c in f.callees.iter().filter(|c| !f.bound.contains(*c)) {
                if let Some(&callee) = out.get(c) {
                    flags = flags.union(callee);
                }
            }
            if flags != out[name] {
                out.insert(name.clone(), flags);
                changed = true;
            }
        }
    }
    out
}

/// Host / virtual-module names that are not user `fn`s.
fn classify_unknown_callee(name: &str) -> EffectFlags {
    classify_host_name(name)
}

/// `by_owner` keys impl methods `Owner::m` (so method calls can resolve to
/// them); otherwise they share the bare name with any same-named `fn`.
fn collect_fns(
    ast: &Output<'_>,
    facts: &mut HashMap<String, FnFacts>,
    by_owner: bool,
    cases: &BareCases,
) {
    match ast.1.as_ref() {
        Expression::Program(items) | Expression::Block(items) | Expression::Fragment(items) => {
            for item in items {
                collect_fns(item, facts, by_owner, cases);
            }
        }
        Expression::Module(_, body) => collect_fns(body, facts, by_owner, cases),
        Expression::Statement(inner)
        | Expression::Expr(inner)
        | Expression::ExprStatement(inner)
        | Expression::Group(inner) => collect_fns(inner, facts, by_owner, cases),
        Expression::Function {
            name,
            args,
            body: Some(body),
            ..
        } => {
            insert_facts(facts, name, fn_facts(args, body, cases));
            collect_nested_fns(body, facts, cases);
        }
        Expression::Implementation { owner, methods, .. } if by_owner => {
            for m in methods {
                if let Some((name, args, body)) = method_fn(m) {
                    insert_facts(facts, &format!("{owner}::{name}"), fn_facts(args, body, cases));
                    collect_nested_fns(body, facts, cases);
                }
            }
        }
        Expression::Implementation { methods, .. } => {
            for m in methods {
                collect_fns(m, facts, by_owner, cases);
            }
        }
        Expression::Method(_, inner) | Expression::Member(inner) => {
            collect_fns(inner, facts, by_owner, cases)
        }
        _ => {}
    }
}

/// The `fn` inside an impl member (through `pub` / attribute wrappers).
fn method_fn<'n, 's>(m: &'n Output<'s>) -> Option<(&'s str, &'n Output<'s>, &'n Output<'s>)> {
    match m.1.as_ref() {
        Expression::Method(_, inner) | Expression::Member(inner) => method_fn(inner),
        Expression::Function {
            name,
            args,
            body: Some(body),
            ..
        } => Some((name, args, body)),
        _ => None,
    }
}

fn collect_nested_fns(ast: &Output<'_>, facts: &mut HashMap<String, FnFacts>, cases: &BareCases) {
    // Walks recurse through here once per level of the tree.
    common::deep_stack(|| collect_nested_fns_level(ast, facts, cases))
}

fn collect_nested_fns_level(ast: &Output<'_>, facts: &mut HashMap<String, FnFacts>, cases: &BareCases) {
    match ast.1.as_ref() {
        Expression::Program(items)
        | Expression::Block(items)
        | Expression::Fragment(items)
        | Expression::Array(items)
        | Expression::Tuple(items) => {
            for item in items {
                collect_nested_fns(item, facts, cases);
            }
        }
        Expression::Function {
            name,
            args,
            body: Some(body),
            ..
        } => {
            insert_facts(facts, name, fn_facts(args, body, cases));
            collect_nested_fns(body, facts, cases);
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
        | Expression::Yield(inner)
        | Expression::YieldFrom(inner)
        | Expression::Panic(inner)
        | Expression::OptionalAccess(inner, _)
        | Expression::Method(_, inner)
        | Expression::Member(inner) => collect_nested_fns(inner, facts, cases),
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
        | Expression::Leq(a, b)
        | Expression::Geq(a, b)
        | Expression::Le(a, b)
        | Expression::Gt(a, b)
        | Expression::Coalesce(a, b)
        | Expression::Assignment(a, b)
        | Expression::CompoundAssign(a, _, b)
        | Expression::Range {
            start: a, end: b, ..
        } => {
            collect_nested_fns(a, facts, cases);
            collect_nested_fns(b, facts, cases);
        }
        Expression::Call { name, args } => {
            collect_nested_fns(name, facts, cases);
            if let Some(args) = args {
                for a in args {
                    collect_nested_fns(a, facts, cases);
                }
            }
        }
        Expression::If(branches) => {
            for b in branches {
                collect_nested_fns(b, facts, cases);
            }
        }
        Expression::Branch(cond, body) => {
            if let Some(c) = cond {
                collect_nested_fns(c, facts, cases);
            }
            collect_nested_fns(body, facts, cases);
        }
        Expression::Match { scrutinee, arms } => {
            collect_nested_fns(scrutinee, facts, cases);
            for arm in arms {
                collect_nested_fns(&arm.body, facts, cases);
            }
        }
        Expression::Construct { fields, .. } => match fields {
            EnumConstructPayload::Tuple(items) => {
                for item in items {
                    collect_nested_fns(item, facts, cases);
                }
            }
            EnumConstructPayload::Record(fields) => {
                for f in fields {
                    collect_nested_fns(&f.value, facts, cases);
                }
            }
            EnumConstructPayload::Unit | EnumConstructPayload::Empty => {}
        },
        Expression::Loop {
            contracts: _,
            identifier,
            pattern: _,
            iterable,
            body,
        } => {
            if let Some(id) = identifier {
                collect_nested_fns(id, facts, cases);
            }
            collect_nested_fns(iterable, facts, cases);
            collect_nested_fns(body, facts, cases);
        }
        Expression::Defer { body, .. } | Expression::Lambda { body, .. } => {
            collect_nested_fns(body, facts, cases);
        }
        Expression::Variable(_, Some(init)) => collect_nested_fns(init, facts, cases),
        Expression::Constant(_, Some(init), ..) => collect_nested_fns(init, facts, cases),
        Expression::LetDestructure { rhs, .. } => collect_nested_fns(rhs, facts, cases),
        Expression::Resume(t, arg) => {
            collect_nested_fns(t, facts, cases);
            if let Some(a) = arg {
                collect_nested_fns(a, facts, cases);
            }
        }
        Expression::Adjust { target, .. } => collect_nested_fns(target, facts, cases),
        Expression::Index(base, Some(idx)) => {
            collect_nested_fns(base, facts, cases);
            collect_nested_fns(idx, facts, cases);
        }
        Expression::Index(base, None) | Expression::Access(base, _) => {
            collect_nested_fns(base, facts, cases);
        }
        Expression::NamedArg(_, v) => collect_nested_fns(v, facts, cases),
        Expression::Implementation { methods, .. } => {
            for m in methods {
                collect_nested_fns(m, facts, cases);
            }
        }
        _ => {}
    }
}

fn walk_body(ast: &Output<'_>, facts: &mut FnFacts) {
    // Walks recurse through here once per level of the tree.
    common::deep_stack(|| walk_body_level(ast, facts))
}

fn walk_body_level(ast: &Output<'_>, facts: &mut FnFacts) {
    match ast.1.as_ref() {
        Expression::Program(items)
        | Expression::Block(items)
        | Expression::Fragment(items)
        | Expression::Array(items)
        | Expression::Tuple(items) => {
            for item in items {
                walk_body(item, facts);
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
        | Expression::LogicalNot(inner)
        | Expression::Cast(inner, _)
        | Expression::TypeOf(inner)
        | Expression::Readonly(inner)
        | Expression::OptionalAccess(inner, _) => walk_body(inner, facts),
        Expression::Negate(inner) | Expression::Not(inner) | Expression::Positive(inner) => {
            facts.operands.push((inner.0.start, inner.0.end));
            walk_body(inner, facts);
        }
        // The resumer (or the resumed body) runs arbitrary code.
        Expression::Yield(_) | Expression::YieldFrom(_) | Expression::Resume(_, _) => {
            facts.local.insert(EffectFlags::YIELD | EffectFlags::RESIZE);
        }
        Expression::Declare(_) | Expression::Invoke(_) => {
            facts.local.insert(EffectFlags::FFI | EffectFlags::RESIZE);
        }
        // Still walked: calls inside the message / deferred body count.
        Expression::Panic(inner) | Expression::Defer { body: inner, .. } => {
            facts.local.insert(EffectFlags::UNKNOWN);
            walk_body(inner, facts);
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
        | Expression::BitAnd(a, b)
        | Expression::BitOr(a, b)
        | Expression::Eq(a, b)
        | Expression::Neq(a, b)
        | Expression::Leq(a, b)
        | Expression::Geq(a, b)
        | Expression::Le(a, b)
        | Expression::Gt(a, b) => {
            facts.operands.push((a.0.start, a.0.end));
            facts.operands.push((b.0.start, b.0.end));
            walk_body(a, facts);
            walk_body(b, facts);
        }
        Expression::And(a, b)
        | Expression::Or(a, b)
        | Expression::Coalesce(a, b)
        | Expression::Range {
            start: a, end: b, ..
        } => {
            walk_body(a, facts);
            walk_body(b, facts);
        }
        Expression::Identifier(n) => {
            facts.free.insert((*n).to_string());
        }
        Expression::Assignment(lhs, rhs) | Expression::CompoundAssign(lhs, _, rhs) => {
            if writes_shared_heap(lhs, &facts.private) {
                facts.local.insert(EffectFlags::HEAP_MUT);
            }
            if let Expression::Identifier(n) = peel(lhs).1.as_ref() {
                facts.free_writes.insert((*n).to_string());
            }
            if matches!(ast.1.as_ref(), Expression::CompoundAssign(..)) {
                facts.operands.push((lhs.0.start, lhs.0.end));
                facts.operands.push((rhs.0.start, rhs.0.end));
            }
            walk_body(lhs, facts);
            walk_body(rhs, facts);
        }
        Expression::Adjust { target, .. } => {
            if writes_shared_heap(target, &facts.private) {
                facts.local.insert(EffectFlags::HEAP_MUT);
            }
            if let Expression::Identifier(n) = peel(target).1.as_ref() {
                facts.free_writes.insert((*n).to_string());
            }
            walk_body(target, facts);
        }
        Expression::Call { name, args } => {
            match peel(name).1.as_ref() {
                Expression::Identifier(n) => {
                    facts.callees.insert((*n).to_string());
                }
                Expression::QualifiedAccess { owner, member } => {
                    facts.callees.insert(format!("{owner}::{member}"));
                }
                // `(recv.field)(..)` is no method call: it calls whatever
                // function the field holds (#2472).
                Expression::Access(recv, member) if matches!(name.1.as_ref(), Expression::Access(..)) => {
                    facts
                        .method_calls
                        .push(((recv.0.start, recv.0.end), (*member).to_string()));
                }
                _ => {
                    facts.local.insert(EffectFlags::UNKNOWN | EffectFlags::RESIZE);
                }
            }
            walk_body(name, facts);
            if let Some(args) = args {
                for a in args {
                    walk_body(a, facts);
                }
            }
        }
        Expression::If(branches) => {
            for b in branches {
                walk_body(b, facts);
            }
        }
        Expression::Branch(cond, body) => {
            if let Some(c) = cond {
                walk_body(c, facts);
            }
            walk_body(body, facts);
        }
        Expression::Match { scrutinee, arms } => {
            walk_body(scrutinee, facts);
            for arm in arms {
                walk_body(&arm.body, facts);
            }
        }
        // Constructor payloads hold arbitrary expressions — skipping them hid
        // both impure calls and enum-building self-recursion.
        Expression::Construct { enum_name, variant_name, fields } => match fields {
            EnumConstructPayload::Tuple(items) => {
                facts.constructs.push(((*enum_name).to_string(), (*variant_name).to_string(), false));
                for item in items {
                    walk_body(item, facts);
                }
            }
            EnumConstructPayload::Record(fields) => {
                for f in fields {
                    walk_body(&f.value, facts);
                }
            }
            // A static method's value (bare) counts as its call: the
            // value is called somewhere.
            EnumConstructPayload::Unit | EnumConstructPayload::Empty => {
                facts.constructs.push(((*enum_name).to_string(), (*variant_name).to_string(), true));
            }
        },
        Expression::Loop {
            contracts: _,
            identifier,
            pattern,
            iterable,
            body,
        } => {
            if identifier.is_some() || pattern.is_some() {
                facts.for_ins.push((ast.0.start, ast.0.end));
            }
            if let Some(id) = identifier {
                walk_body(id, facts);
            }
            walk_body(iterable, facts);
            walk_body(body, facts);
        }
        Expression::Variable(_, Some(init)) => walk_body(init, facts),
        Expression::Constant(_, Some(init), ..) => walk_body(init, facts),
        Expression::LetDestructure { rhs, .. } => walk_body(rhs, facts),
        Expression::Lambda { .. } => {
            facts.local.insert(EffectFlags::UNKNOWN);
        }
        Expression::Function {
            body: Some(body), ..
        } => {
            walk_body(body, facts);
        }
        Expression::Index(base, Some(idx)) => {
            walk_body(base, facts);
            walk_body(idx, facts);
        }
        Expression::Index(base, None) | Expression::Access(base, _) => walk_body(base, facts),
        Expression::NamedArg(_, v) => walk_body(v, facts),
        // `if let`, `while let`, `new`, and anything added later: effects in
        // any child count. Skipping them used to hide calls from purity.
        _ => walk_children(ast, &mut |c| walk_body(c, facts)),
    }
}

/// True when assigning to `lhs` writes heap memory a caller could see: an
/// index or field write whose base is not one of the function's `private`
/// locals.
fn writes_shared_heap(lhs: &Output<'_>, private: &HashSet<String>) -> bool {
    match peel(lhs).1.as_ref() {
        Expression::Index(base, _) | Expression::Access(base, _) => {
            !matches!(peel(base).1.as_ref(), Expression::Identifier(n) if private.contains(*n))
        }
        _ => false,
    }
}

/// Locals whose object is created in this function and never shared, so
/// writing through them is invisible to callers. A name qualifies when:
///
/// - it is bound exactly once in the function (no shadowing, not a param),
///   by `let x = <array, list, tuple, dict or class literal>`;
/// - every use is `x[i]`, `x.f`, a write through one of those, or
///   `return x` (the caller gets the object only after the last write);
/// - it is never reassigned, captured by a lambda, passed to a call, stored
///   elsewhere, or iterated.
///
/// Only writes directly through `x` count: `x[i][j] = v` writes into an
/// element, which may be shared.
fn private_locals(args: &Output<'_>, body: &Output<'_>, cases: &BareCases) -> HashSet<String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    each_binder(args, cases, &mut |n| *counts.entry(n.to_string()).or_default() += 1);
    each_binder(body, cases, &mut |n| *counts.entry(n.to_string()).or_default() += 1);
    let mut fresh = HashSet::new();
    fresh_lets(body, &mut fresh);
    fresh.retain(|n: &String| counts.get(n) == Some(&1));
    if fresh.is_empty() {
        return fresh;
    }
    let mut escaped = HashSet::new();
    private_uses(body, &fresh, &mut escaped);
    fresh.retain(|n| !escaped.contains(n));
    fresh
}

/// Names bound by `let x = <fresh literal>` anywhere in `ast`. The parser
/// spells `let x = e` as `Fragment([Variable(x, ty), e])`.
fn fresh_lets(ast: &Output<'_>, out: &mut HashSet<String>) {
    if let Expression::Fragment(items) = ast.1.as_ref()
        && let [decl, init] = items.as_slice()
        && let Expression::Variable(n, _) = decl.1.as_ref()
        && matches!(
            peel(init).1.as_ref(),
            Expression::Array(_)
                | Expression::Tuple(_)
                | Expression::Dict(_)
                | Expression::Instantiate(_, _)
        )
    {
        out.insert((*n).to_string());
    }
    walk_children(ast, &mut |c| fresh_lets(c, out));
}

/// Mark every name in `fresh` that `ast` uses other than through `x[i]`,
/// `x.f`, a write through one, or `return x`.
fn private_uses(ast: &Output<'_>, fresh: &HashSet<String>, escaped: &mut HashSet<String>) {
    // Walks recurse through here once per level of the tree.
    common::deep_stack(|| private_uses_level(ast, fresh, escaped))
}

fn private_uses_level(ast: &Output<'_>, fresh: &HashSet<String>, escaped: &mut HashSet<String>) {
    let base_name = |e: &Output<'_>| match peel(e).1.as_ref() {
        Expression::Identifier(n) if fresh.contains(*n) => Some((*n).to_string()),
        _ => None,
    };
    match ast.1.as_ref() {
        Expression::Identifier(n) => {
            if fresh.contains(*n) {
                escaped.insert((*n).to_string());
            }
        }
        Expression::Index(base, idx) if base_name(base).is_some() => {
            if let Some(i) = idx {
                private_uses(i, fresh, escaped);
            }
        }
        Expression::Access(base, _) if base_name(base).is_some() => {}
        // A method call passes the receiver along: the callee may keep it.
        Expression::Call { name, args } => {
            if let Expression::Access(base, _) = peel(name).1.as_ref()
                && let Some(n) = base_name(base)
            {
                escaped.insert(n);
            }
            private_uses(name, fresh, escaped);
            if let Some(args) = args {
                for a in args {
                    private_uses(a, fresh, escaped);
                }
            }
        }
        Expression::Return(inner) | Expression::ImplicitReturn(inner)
            if base_name(inner).is_some() => {}
        Expression::Lambda { captures, .. } => {
            for c in captures {
                if fresh.contains(*c) {
                    escaped.insert((*c).to_string());
                }
            }
            walk_children(ast, &mut |c| private_uses(c, fresh, escaped));
        }
        _ => walk_children(ast, &mut |c| private_uses(c, fresh, escaped)),
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

/// The effects of this file's functions (keyed `f`) and impl methods
/// (keyed `Owner::m`), settled with the checker's types and with what the
/// modules checked before it recorded.
fn checked_fn_effects(checker: &super::infer::Checker, ast: &Output<'_>) -> HashMap<String, EffectFlags> {
    let resolve = |name: &str| {
        if let Some(&fx) = checker.program_method_effects.get(name) {
            return Some(fx);
        }
        let id = checker.def_id_of(name).or_else(|| {
            let (module, short) = name.rsplit_once("::")?;
            checker.interned_def(module, short)
        })?;
        checker.program_fn_effects.get(&id).copied()
    };
    let mut facts = HashMap::new();
    collect_fns(ast, &mut facts, true, checker.bare_cases());
    let target = |span: (usize, usize), m: &str| {
        let owner = checker.class_owner_at_span(span)?;
        let local = owner
            .strip_prefix(checker.current_module_name())
            .and_then(|o| o.strip_prefix("::"))
            .unwrap_or(&owner);
        Some(format!("{local}::{m}"))
    };
    settle_method_calls(&mut facts, &target);
    settle_statics(&mut facts, ast);
    let operand_runs = |span| crate::effects::operator_runs_user_code(checker.ty_at_span(span).as_ref());
    let for_in_runs = |(start, end)| {
        use super::infer::ForInKind;
        !matches!(
            checker.for_in_info_for_span(start, end).map(|i| &i.kind),
            Some(ForInKind::Array | ForInKind::Tuple { .. } | ForInKind::Range { .. })
        )
    };
    settle_operators(&mut facts, &operand_runs, &for_in_runs);
    settle_constructs(&mut facts, &|owner, name| checker.tag_for(owner, name).is_some(), &|owner, name| {
        super::index_facts::static_method_call(checker, owner, name)
    });
    effect_closure_with(&facts, &resolve)
}

/// Fill [`Checker::length_stability`] after infer: what the checker's
/// bounds proofs (`index_facts`) may assume of a call. They run before
/// there is any HIR, so this walk is their only source.
///
/// The effects are also kept for the whole program (per [`DefId`] for a
/// function, by `module::Owner::m` for a method), so the walk of a module
/// checked later sees an imported function's effects instead of treating
/// the call as unknown (modules are checked in dependency order).
///
/// [`DefId`]: super::def_id::DefId
pub fn record_length_stability(checker: &mut super::infer::Checker, ast: &Output<'_>) {
    let effects = checked_fn_effects(checker, ast);
    checker.length_stability = length_stability(&effects, checker.program_finalizers_resize.unwrap_or(false));
    let module = checker.current_module_name().to_string();
    for (name, flags) in effects {
        if name.contains("::") {
            let qualified = if module.is_empty() { name } else { format!("{module}::{name}") };
            checker.program_method_effects.insert(qualified, flags);
        } else if let Some(id) = checker.def_id_of(&name) {
            checker.program_fn_effects.insert(id, flags);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parser::Pratt;

    /// A lone file's pure functions: the ones that stay length-stable when
    /// a finalizer may resize.
    fn analyze_pure_fns(ast: &Output<'_>, cases: &BareCases) -> HashSet<String> {
        length_stability(&analyze_fn_effects(ast, cases), true).fns
    }

    /// The walk's verdicts for the file a checker checked last, by the
    /// walk's keys (`f`, `Owner::m`).
    struct Checked(HashMap<String, EffectFlags>);

    impl Checked {
        fn of(checker: &crate::typechecking::infer::Checker, ast: &Output<'_>) -> Self {
            Self(checked_fn_effects(checker, ast))
        }

        fn name_is_pure(&self, name: &str) -> bool {
            self.0.get(name).is_some_and(|f| f.is_pure())
        }

        fn fn_effects(&self) -> &HashMap<String, EffectFlags> {
            &self.0
        }
    }

    fn pure_set(src: &str) -> HashSet<String> {
        let owned = src.to_string();
        let ast = Pratt::default().parse(owned.as_str()).expect("parse");
        analyze_pure_fns(&ast, BareCases::none())
    }

    #[test]
    fn static_reads_and_writes_are_not_pure() {
        let ast = parse_ast(
            r#"
static let N: int = 0;
static const K: int = 2;
fn reads(int x) -> int { return x + N; }
fn writes(int x) -> int { N = x; return x; }
fn bumps(int x) -> int { N++; return x; }
fn konst(int x) -> int { return x * K; }
fn shadow(int N) -> int { return N + 1; }
fn main() { return; }
"#,
        );
        let pure = analyze_pure_fns(&ast, BareCases::none());
        assert!(!pure.contains("reads"), "{pure:?}");
        assert!(!pure.contains("writes"));
        assert!(!pure.contains("bumps"));
        assert!(pure.contains("konst"));
        assert!(pure.contains("shadow"));
    }

    /// A module `const` that does not fold is a slot nothing writes (#1558):
    /// reading it keeps a function pure.
    #[test]
    fn startup_const_reads_are_pure() {
        let owned = r#"
fn two() -> int { return 2; }
const K: int = two();
fn konst(int x) -> int { return x * K; }
fn main() { return; }
"#
        .to_string();
        let mut ast = Pratt::default().parse(owned.as_str()).expect("parse");
        crate::const_fold::lift_startup_consts(&mut ast);
        let pure = analyze_pure_fns(&ast, BareCases::none());
        assert!(pure.contains("konst"), "{pure:?}");
    }

    // #1394: a bare unit case in a pattern (`Red`) binds nothing, so it does
    // not shadow a same-named static; an uppercase name no enum declares
    // (`N`) still binds, as in the checker.
    #[test]
    fn bare_unit_case_pattern_is_not_a_binder() {
        let ast = parse_ast(
            r#"
static let Red: int = 0;
static let N: int = 0;
enum Color {
    Red,
    Green,
}
fn case_arm(Color c) -> int {
    return match c {
        Red => Red,
        default => 1,
    };
}
fn none_arm(Option<int> o) -> int {
    return match o {
        None => N,
        Some(v) => v,
    };
}
fn binding_arm(int x) -> int {
    return match x {
        N => N + 1,
    };
}
fn main() { return; }
"#,
        );
        let pure = analyze_pure_fns(&ast, &BareCases::checked(&ast));
        assert!(!pure.contains("case_arm"), "`Red` reads the static: {pure:?}");
        assert!(!pure.contains("none_arm"), "`N` reads the static: {pure:?}");
        assert!(pure.contains("binding_arm"), "`N` binds the scrutinee: {pure:?}");
    }

    #[test]
    fn recursive_fib_is_pure() {
        let set = pure_set(
            r#"
fn fib(int n) -> int {
    if n <= 1 { return n; }
    return fib(n - 1) + fib(n - 2);
}
fn main() { return; }
"#,
        );
        assert!(set.contains("fib"), "fib should be pure: {set:?}");
    }

    fn parse_ast(src: &str) -> parser::ast::Output<'static> {
        let owned = Box::leak(src.to_string().into_boxed_str());
        Pratt::default().parse(owned).expect("parse")
    }

    #[test]
    fn io_fn_is_not_pure() {
        let set = pure_set(
            r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn speak(int n) -> int {
    write(stdout(), to_bytes(format("%i", n)));
    return n;
}
fn main() { return; }
"#,
        );
        assert!(!set.contains("speak"), "speak uses IO: {set:?}");
    }

    /// A helper that does not recurse is pure all the same.
    #[test]
    fn non_recursive_helpers_are_pure() {
        let ast = parse_ast(
            r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn add(int a, int b) -> int { return a + b; }
fn shout(int n) -> int {
    write(stdout(), to_bytes(format("%i", n)));
    return n;
}
fn relay(int n) -> int { return shout(n); }
fn main() { return; }
"#,
        );
        let set = analyze_pure_fns(&ast, BareCases::none());
        assert!(set.contains("add"), "{set:?}");
        assert!(!set.contains("shout"), "{set:?}");
        assert!(!set.contains("relay"), "impurity propagates: {set:?}");
    }

    #[test]
    fn impurity_propagates_through_callees() {
        let set = pure_set(
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
        assert!(!set.contains("leaf"), "leaf uses IO: {set:?}");
        assert!(
            !set.contains("rec"),
            "rec must not be pure when it reaches impure leaf: {set:?}"
        );
    }

    #[test]
    fn index_store_marks_function_impure() {
        let set = pure_set(
            r#"
fn bump(Vec<int> a, int n) -> int {
    a[0] = n;
    if n <= 1 { return a[0]; }
    return bump(a, n - 1) + bump(a, n - 2);
}
fn main() { return; }
"#,
        );
        assert!(
            !set.contains("bump"),
            "index store into a param is a side effect: {set:?}"
        );
    }

    #[test]
    fn vec_push_helper_is_not_pure() {
        let ast = parse_ast(
            r#"
fn grow(Vec<int> a, int x) {
    a.push(x);
}
fn main() { return; }
"#,
        );
        let set = analyze_pure_fns(&ast, BareCases::none());
        assert!(
            !set.contains("grow"),
            "ArrayPush through a helper must be impure: {set:?}"
        );
    }

    #[test]
    fn loop_helper_without_effects_is_pure() {
        let ast = parse_ast(
            r#"
fn absorb(int x) -> int {
    let t = 0;
    let k = 0;
    while k < x {
        t = t + 1;
        k = k + 1;
    }
    return t;
}
fn main() { return; }
"#,
        );
        let set = analyze_pure_fns(&ast, BareCases::none());
        assert!(
            set.contains("absorb"),
            "counted helper with no host/push must stay pure: {set:?}"
        );
    }

    /// Skipping `Construct` payloads hid IO inside `Tree::Node(shout(n), …)`.
    #[test]
    fn impure_call_in_enum_ctor_payload_marks_impure() {
        let set = pure_set(
            r#"
use io::{stdout, write};
use string::{format, to_bytes};
enum Box {
    Wrap(int),
}
fn shout(int n) -> int {
    write(stdout(), to_bytes(format("%i", n)));
    return n;
}
fn pack(int n) -> Box {
    if n <= 0 { return Box::Wrap(0); }
    return Box::Wrap(shout(n));
}
fn main() { return; }
"#,
        );
        assert!(!set.contains("shout"), "shout uses IO: {set:?}");
        assert!(
            !set.contains("pack"),
            "impurity in a constructor payload must reach the enclosing fn: {set:?}"
        );
    }

    /// Self-calls that only appear inside `Tree::Node(…)` are walked, and the
    /// builder stays pure so EnumCtor IPA can fire.
    #[test]
    fn self_recursion_only_via_enum_ctor_is_pure() {
        let set = pure_set(
            r#"
enum Tree {
    Leaf,
    Node(Tree, Tree),
}
fn build(int n) -> Tree {
    if n <= 1 { return Tree::Leaf; }
    return Tree::Node(build(n - 1), build(n - 2));
}
fn main() { return; }
"#,
        );
        assert!(
            set.contains("build"),
            "enum-building self-recursion must be pure: {set:?}"
        );
    }

    /// Record-payload constructors are walked the same way as tuple ones.
    #[test]
    fn impure_call_in_record_enum_ctor_payload_marks_impure() {
        let set = pure_set(
            r#"
use io::{stdout, write};
use string::{format, to_bytes};
enum Cell {
    Val { x: int },
}
fn shout(int n) -> int {
    write(stdout(), to_bytes(format("%i", n)));
    return n;
}
fn pack(int n) -> Cell {
    return Cell::Val { x: shout(n) };
}
fn main() { return; }
"#,
        );
        assert!(
            !set.contains("pack"),
            "record ctor payloads must not hide impurity: {set:?}"
        );
    }

    #[test]
    fn float_helper_is_pure_clock_helper_is_not() {
        let ast = parse_ast(
            r#"
use clock::{mono_nanos};
fn wave(float x) -> float { return x * x - 1.0; }
fn tick() -> int { return mono_nanos(); }
fn main() { return; }
"#,
        );
        let set = analyze_pure_fns(&ast, BareCases::none());
        assert!(
            set.contains("wave"),
            "float arithmetic must not poison purity: {set:?}"
        );
        assert!(!set.contains("tick"), "clocks are observational: {set:?}");
    }

    #[test]
    fn the_checked_walk_records_a_pure_helper_and_a_host_callee() {
        use crate::typechecking::infer::Checker;

        let ast = parse_ast(
            r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn add(int a, int b) -> int { return a + b; }
fn shout(int n) -> int {
    write(stdout(), to_bytes(format("%i", n)));
    return n;
}
fn main() { return; }
"#,
        );
        let mut c = Checker::new();
        let _ = c.check_program(&ast);
        let side = Checked::of(&c, &ast);
        assert!(side.name_is_pure("add"), "add must stay pure");
        assert!(!side.name_is_pure("shout"), "host write must be impure");
        assert!(side.fn_effects()["add"].is_pure());
        let shout_fx = side.fn_effects()["shout"];
        assert!(
            shout_fx.contains(EffectFlags::IO) || shout_fx.contains(EffectFlags::UNKNOWN),
            "shout should record IO/unknown, got {shout_fx:?}"
        );
    }

    /// `Owner::name(..)` parses like an enum constructor; one that is not
    /// is a call, with the effects of what it calls (#2407).
    #[test]
    fn construct_shaped_calls_carry_their_effects() {
        use crate::typechecking::infer::Checker;

        let ast = parse_ast(
            r#"
static let CALLS: int = 0;
enum Shape {
    Dot,
    Circle(int),
}
class Counter {
    pub n: int,
    static count: int = 0,
}
impl Counter {
    pub static fn quiet(int n) -> int { return n + 1; }
    pub static fn loud(int n) -> int {
        CALLS++;
        return n;
    }
}
trait Make<T> {
    static fn make(int n) -> T {}
}
impl Make for Shape {
    pub static fn make(int n) -> Shape {
        CALLS++;
        return Shape::Circle(n);
    }
}
fn variant(int n) -> Shape { return Shape::Circle(n); }
fn unit_variant() -> Shape { return Shape::Dot; }
fn some(int n) -> Option<int> { return Option::Some(n); }
fn inherent(int n) -> int { return Counter::quiet(n); }
fn inherent_loud(int n) -> int { return Counter::loud(n); }
fn built(int n) -> Shape { return Shape::make(n); }
fn made<T: Make>(int n) -> T { return T::make(n); }
fn reads() -> int { return Counter::count; }
fn main() { return; }
"#,
        );
        let mut c = Checker::new();
        let _ = c.check_program(&ast);
        let side = Checked::of(&c, &ast);
        for name in ["variant", "unit_variant", "some", "inherent"] {
            assert!(side.name_is_pure(name), "`{name}` only builds a value or calls a pure method");
        }
        for name in ["inherent_loud", "built", "made", "reads"] {
            assert!(!side.name_is_pure(name), "`{name}` is not pure");
        }
    }

    /// An operator on a class or a type parameter, and a `for … in` over a
    /// user iterator, run user code; on built-in types they do not (#2388).
    #[test]
    fn user_operators_and_iterators_are_not_pure() {
        use crate::typechecking::infer::Checker;

        let ast = parse_ast(
            r#"
class Money {
    pub cents: int,
}
impl Lt for Money {
    pub fn lt(Money a, Money b) -> bool { return a.cents < b.cents; }
}
impl Neg for Money {
    pub fn neg(Money a) -> Money { return new Money(0 - a.cents); }
}
impl Add for Money {
    pub fn add(Money a, Money b) -> Money { return new Money(a.cents + b.cents); }
}
class Count {
    pub cur: int,
}
impl Iterator for Count {
    type Item = int;
    fn next(Count c) -> Option<int> { return Option::None; }
}
fn less(Money a, Money b) -> bool { return a < b; }
fn less_of<T: Lt>(T a, T b) -> bool { return a < b; }
fn flipped(Money a) -> int { let b = -a; return b.cents; }
fn summed(Money a, Money b) -> int { let c = a; c += b; return c.cents; }
fn drain(Count c) -> int { let s = 0; for x in c { s = s + x; } return s; }
fn via(Money a, Money b) -> bool { return less(a, b); }
fn ints(int a, int b) -> bool { return a < b && -a != b; }
fn strs(string a, string b) -> bool { return a + b == b; }
fn opts(Option<int> a, (int, float) t) -> bool { return a == Option::None && t == (1, 2.0); }
fn each(Vec<int> xs) -> int { let s = 0; for x in xs { s += x; } for i in 0..3 { s += i; } return s; }
fn main() { return; }
"#,
        );
        let mut c = Checker::new();
        let _ = c.check_program(&ast);
        let side = Checked::of(&c, &ast);
        for name in ["less", "less_of", "flipped", "summed", "drain", "via"] {
            assert!(!side.name_is_pure(name), "`{name}` runs user code");
            assert!(!c.typed_sidecar().length_stability().fns.contains(name), "`{name}` may resize");
        }
        for name in ["ints", "strs", "opts", "each"] {
            assert!(side.name_is_pure(name), "`{name}` only uses built-in operators");
        }
    }

    #[test]
    fn imported_fns_carry_their_effects_by_def_id() {
        use crate::typechecking::infer::Checker;

        let mut c = Checker::new();
        c.set_current_module("geom");
        let lib = parse_ast(
            r#"
use io::{stdout, write};
use string::{to_bytes};
fn sq(int x) -> int { return x * x; }
fn shout(int n) -> int {
    write(stdout(), to_bytes("!"));
    return n;
}
fn sin(float x) -> float {
    write(stdout(), to_bytes("?"));
    return x;
}
class Sq {
    pub s: int,
}
impl Sq {
    pub fn area() -> int { return sq(self.s); }
    pub fn say() -> int { return shout(self.s); }
}
"#,
        );
        let _ = c.check_program(&lib);
        c.set_current_module("");
        let app = parse_ast(
            r#"
use geom::{sq, shout, sin, Sq};
fn area(int s) -> int { return sq(s) + 1; }
fn sq_area(Sq q) -> int { return q.area(); }
fn sq_say(Sq q) -> int { return q.say(); }
fn loud(int s) -> int { return shout(s); }
fn wave(float x) -> float { return sin(x); }
fn qualified(int s) -> int { return geom::sq(s); }
fn main() { return; }
"#,
        );
        let _ = c.check_program(&app);
        let side = Checked::of(&c, &app);
        assert!(side.name_is_pure("area"), "pure import keeps the caller pure");
        assert!(side.name_is_pure("qualified"), "qualified call resolves too");
        assert!(!side.name_is_pure("loud"), "impure import stays impure");
        // A user `sin` shadows the host math row of the same short name.
        assert!(!side.name_is_pure("wave"), "user sin does IO");
        assert!(side.name_is_pure("sq_area"), "imported pure method");
        assert!(!side.name_is_pure("sq_say"), "imported impure method");
    }

    #[test]
    fn method_calls_resolve_to_their_impl() {
        use crate::typechecking::infer::Checker;

        let ast = parse_ast(
            r#"
use io::{stdout, write};
use string::{to_bytes};
class Point {
    pub x: int,
    pub y: int,
}
impl Point {
    pub fn norm1() -> int { return self.x + self.y; }
    pub fn twice() -> int { return self.norm1() * 2; }
    pub fn shift(int d) { self.x = self.x + d; }
    pub fn show() -> int {
        write(stdout(), to_bytes("p"));
        return self.x;
    }
}
class Other {
    pub n: int,
}
impl Other {
    pub fn norm1() -> int {
        write(stdout(), to_bytes("o"));
        return self.n;
    }
}
fn size(Point p) -> int { return p.twice() + 1; }
fn nudge(Point p) -> int { p.shift(1); return p.x; }
fn loud(Point p) -> int { return p.show(); }
fn other(Other o) -> int { return o.norm1(); }
fn main() { return; }
"#,
        );
        let mut c = Checker::new();
        let _ = c.check_program(&ast);
        let side = Checked::of(&c, &ast);
        assert!(side.name_is_pure("Point::norm1"));
        assert!(side.name_is_pure("Point::twice"), "self call resolves");
        assert!(side.name_is_pure("size"), "pure method keeps caller pure");
        assert!(!side.name_is_pure("nudge"), "field write through a param");
        assert!(!side.name_is_pure("loud"), "IO method");
        // Same method name on another class: the receiver type decides.
        assert!(!side.name_is_pure("other"));
    }

    /// `(b.f)(1)` calls the function the field `f` holds, whatever a method
    /// `f` of the class does (#2472).
    #[test]
    fn a_parenthesized_field_callee_is_no_method_call() {
        use crate::typechecking::infer::Checker;

        let ast = parse_ast(
            r#"
class Both {
    pub f: fn(int) -> int,
}
impl Both {
    pub fn f(int x) -> int { return x + 100; }
}
fn via_method(Both b) -> int { return b.f(1); }
fn via_field(Both b) -> int { return (b.f)(1); }
fn main() { return; }
"#,
        );
        let mut c = Checker::new();
        let _ = c.check_program(&ast);
        let side = Checked::of(&c, &ast);
        assert!(side.name_is_pure("via_method"), "the pure method");
        assert!(!side.name_is_pure("via_field"), "an unknown function value");
    }

    /// `if let` bodies used to be skipped, hiding IO from purity.
    #[test]
    fn if_let_body_effects_count() {
        let set = analyze_pure_fns(&parse_ast(
            r#"
use io::{stdout, write};
use string::{format, to_bytes};
fn speak(Option<int> o) -> int {
    if let Option::Some(n) = o {
        write(stdout(), to_bytes(format("%i", n)));
    }
    return 0;
}
fn main() { return; }
"#,
        ), BareCases::none());
        assert!(!set.contains("speak"), "speak writes under if let: {set:?}");
    }

    fn stability(src: &str) -> LengthStability {
        let ast = parse_ast(src);
        length_stability(&analyze_fn_effects(&ast, BareCases::none()), false)
    }

    /// Field and element writes are impure but cannot change an array length.
    #[test]
    fn field_writer_is_length_stable_but_impure() {
        let src = r#"
class Tally {
    pub hits: int,
}
fn absorb(Tally t, int x) -> int {
    t.hits = t.hits + 1;
    return x;
}
fn poke(Vec<int> v) -> int {
    v[0] = 1;
    return 0;
}
fn main() { return; }
"#;
        let st = stability(src);
        assert!(st.alloc_stable);
        assert!(st.fns.contains("absorb"), "{:?}", st.fns);
        assert!(st.fns.contains("poke"), "{:?}", st.fns);
        assert!(!analyze_pure_fns(&parse_ast(src), BareCases::none()).contains("absorb"));
    }

    #[test]
    fn push_or_unknown_method_is_not_length_stable() {
        let st = stability(
            r#"
fn grow(Vec<int> v) -> int {
    v.push(1);
    return 0;
}
fn via(Vec<int> v) -> int {
    return grow(v);
}
fn size(Vec<int> v) -> int {
    return v.len();
}
fn main() { return; }
"#,
        );
        assert!(!st.fns.contains("grow"), "{:?}", st.fns);
        assert!(!st.fns.contains("via"), "callee resizes: {:?}", st.fns);
        assert!(st.fns.contains("size"), "{:?}", st.fns);
    }

    /// A parameter that shadows a length-stable host name is a function value.
    #[test]
    fn shadowed_host_name_is_not_length_stable() {
        let st = stability(
            r#"
fn apply(Handler write, int x) -> int {
    return write(x);
}
fn main() { return; }
"#,
        );
        assert!(!st.fns.contains("apply"), "{:?}", st.fns);
    }

    /// Same-named methods in two impls share one entry; the resizing body
    /// must not be overwritten by the stable one.
    #[test]
    fn same_named_methods_union_their_effects() {
        let st = stability(
            r#"
class A {
    pub xs: Vec<int>,
}
class B {
    pub n: int,
}
impl A {
    fn step() -> int {
        self.xs.push(1);
        return 0;
    }
}
impl B {
    fn step() -> int {
        return self.n;
    }
}
fn main() { return; }
"#,
        );
        assert!(!st.fns.contains("step"), "{:?}", st.fns);
    }

    /// A finalizer that can resize makes allocation a barrier, so only pure
    /// functions stay length-stable.
    #[test]
    fn resizing_finalizer_falls_back_to_pure() {
        let st = stability(
            r#"
class Log {
    pub xs: Vec<int>,
}
class Tally {
    pub hits: int,
}
impl Log {
    fn drop() {
        self.xs.push(1);
    }
}
fn absorb(Tally t, int x) -> int {
    t.hits = t.hits + 1;
    return x;
}
fn main() { return; }
"#,
        );
        assert!(!st.alloc_stable);
        assert!(!st.fns.contains("absorb"), "{:?}", st.fns);
    }

    #[test]
    fn writes_to_a_private_local_are_pure() {
        let set = pure_set(
            r#"
fn rec(int n) -> int {
    if n <= 1 { return n; }
    let a = [0, 0];
    a[0] = rec(n - 1);
    a[1] = rec(n - 2);
    return a[0] + a[1];
}
fn main() { return; }
"#,
        );
        assert!(set.contains("rec"), "{set:?}");
    }

    #[test]
    fn writes_that_a_caller_could_see_stay_impure() {
        for (why, body) in [
            ("param", "fn rec(Vec<int> xs, int n) -> int { if n <= 1 { return n; } xs[0] = n; return rec(xs, n - 1) + rec(xs, n - 2); }"),
            ("alias", "fn rec(int n) -> int { if n <= 1 { return n; } let a = [0]; let b = a; b[0] = n; return rec(n - 1) + rec(n - 2); }"),
            ("stored", "fn rec(int n) -> int { if n <= 1 { return n; } let a = [0]; let b = [a]; a[0] = n; return rec(n - 1) + rec(n - 2) + b[0][0]; }"),
            ("nested", "fn rec(Vec<int> xs, int n) -> int { if n <= 1 { return n; } let a = [xs]; a[0][0] = n; return rec(xs, n - 1) + rec(xs, n - 2); }"),
            ("shadowed", "fn rec(Vec<int> a, int n) -> int { if n <= 1 { return n; } if n > 5 { let a = [0]; a[0] = 1; } a[0] = n; return rec(a, n - 1) + rec(a, n - 2); }"),
            ("reassigned", "fn rec(Vec<int> xs, int n) -> int { if n <= 1 { return n; } let a = [0]; a = xs; a[0] = n; return rec(xs, n - 1) + rec(xs, n - 2); }"),
        ] {
            let set = pure_set(&format!("{body}\nfn main() {{ return; }}\n"));
            assert!(!set.contains("rec"), "{why}: {set:?}");
        }
    }
}
