//! Whole-function purity / effects for auto-par, LICM, and the typed sidecar.
//!
//! A function is **pure** when its body has no observable host side effects
//! (IO / threads / FFI / yield / attach) and only calls other pure user
//! functions. Unknown callees are conservatively impure. **Recursive pure**
//! functions may be auto-parallelized at `f(a) ⊕ f(b)` sites.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use parser::ast::{EnumConstructPayload, Expression, LetPattern, Output, Pattern, PatternPayload};

use super::id::walk_children;
use super::virtual_modules::{BuiltinExport, PreludeFn, VirtualModules};

/// Names of user functions that are pure and self-recursive.
pub type RecursivePureSet = HashSet<String>;

pub use common::EffectFlags;

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
}

/// Turn each fn's `recv.m(..)` calls into callees. `target` names the impl
/// method (`Owner::m`) for a receiver span when its type is a user class;
/// anything else stays unknown, except `len` / `capacity`, which cannot resize.
fn settle_method_calls(
    facts: &mut HashMap<String, FnFacts>,
    target: &dyn Fn((usize, usize), &str) -> Option<String>,
) {
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

/// Every name `ast` binds, at any depth: params, `let`, `const`, for-in and
/// match / `if let` pattern bindings.
fn collect_binders(ast: &Output<'_>, out: &mut HashSet<String>) {
    each_binder(ast, &mut |n| {
        out.insert(n.to_string());
    });
}

/// Call `f` once per binding site in `ast` (see [`collect_binders`]).
fn each_binder(ast: &Output<'_>, f: &mut dyn FnMut(&str)) {
    let mut here: HashSet<String> = HashSet::new();
    let out = &mut here;
    match ast.1.as_ref() {
        Expression::Variable(n, _) | Expression::Argument { name: n, .. } => {
            out.insert((*n).to_string());
        }
        Expression::Constant(name, _) => {
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
                pattern_binders(&arm.pattern.1, out);
            }
        }
        Expression::IfLet {
            then_arm, else_arm, ..
        } => {
            pattern_binders(&then_arm.pattern.1, out);
            pattern_binders(&else_arm.pattern.1, out);
        }
        Expression::WhileLet {
            then_arm, on_miss, ..
        } => {
            pattern_binders(&then_arm.pattern.1, out);
            pattern_binders(&on_miss.pattern.1, out);
        }
        _ => {}
    }
    for n in &here {
        f(n);
    }
    walk_children(ast, &mut |c| each_binder(c, f));
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

fn pattern_binders(p: &Pattern<'_>, out: &mut HashSet<String>) {
    match p {
        Pattern::Binding { name } => {
            out.insert((*name).to_string());
        }
        Pattern::Constructor { payload, .. } => match payload {
            PatternPayload::Unit => {}
            PatternPayload::Tuple(items) => {
                for (_, item) in items {
                    pattern_binders(item, out);
                }
            }
            PatternPayload::Record(fields) => {
                for f in fields {
                    pattern_binders(&f.pattern.1, out);
                }
            }
        },
        Pattern::Wildcard | Pattern::Default | Pattern::Integer(_) => {}
    }
}

/// Facts for one `fn` declaration: its body effects plus every bound name.
fn fn_facts(args: &Output<'_>, body: &Output<'_>) -> FnFacts {
    let mut f = FnFacts {
        private: private_locals(args, body),
        ..FnFacts::default()
    };
    walk_body(body, &mut f);
    collect_binders(args, &mut f.bound);
    collect_binders(body, &mut f.bound);
    f
}

/// Collect per-function callee sets (and local impurity) for call-graph analyses.
fn collect_fn_facts(ast: &Output<'_>) -> HashMap<String, FnFacts> {
    let mut facts: HashMap<String, FnFacts> = HashMap::new();
    collect_fns(ast, &mut facts, false);
    settle_method_calls(&mut facts, &|_, _| None);
    facts
}

/// Names of user functions that appear in a call-graph cycle (self or mutual).
///
/// Only **top-level / module** `fn`s are considered. Impl methods are skipped:
/// an Identifier call equal to the method name usually resolves to an imported
/// free function (`join(self.thread)`), not a self-call.
pub fn analyze_recursive_fns(ast: &Output<'_>) -> HashSet<String> {
    let mut facts: HashMap<String, FnFacts> = HashMap::new();
    collect_toplevel_fns(ast, &mut facts);
    let user_fns: HashSet<String> = facts.keys().cloned().collect();
    let mut in_cycle = HashSet::new();

    for (name, f) in &facts {
        if f.callees.contains(name) {
            in_cycle.insert(name.clone());
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Color {
        White,
        Gray,
        Black,
    }
    let mut color: HashMap<String, Color> =
        user_fns.iter().map(|n| (n.clone(), Color::White)).collect();
    let mut stack: Vec<String> = Vec::new();

    fn dfs(
        name: &str,
        facts: &HashMap<String, FnFacts>,
        user_fns: &HashSet<String>,
        color: &mut HashMap<String, Color>,
        stack: &mut Vec<String>,
        in_cycle: &mut HashSet<String>,
    ) {
        color.insert(name.to_string(), Color::Gray);
        stack.push(name.to_string());
        if let Some(f) = facts.get(name) {
            for c in &f.callees {
                if !user_fns.contains(c) {
                    continue;
                }
                match color.get(c).copied().unwrap_or(Color::White) {
                    Color::White => dfs(c, facts, user_fns, color, stack, in_cycle),
                    Color::Gray => {
                        if let Some(start) = stack.iter().position(|n| n == c) {
                            for n in &stack[start..] {
                                in_cycle.insert(n.clone());
                            }
                        }
                    }
                    Color::Black => {}
                }
            }
        }
        stack.pop();
        color.insert(name.to_string(), Color::Black);
    }

    for name in &user_fns {
        if color.get(name) == Some(&Color::White) {
            dfs(
                name,
                &facts,
                &user_fns,
                &mut color,
                &mut stack,
                &mut in_cycle,
            );
        }
    }
    in_cycle
}

fn collect_toplevel_fns(ast: &Output<'_>, facts: &mut HashMap<String, FnFacts>) {
    match ast.1.as_ref() {
        Expression::Program(items) | Expression::Block(items) | Expression::Fragment(items) => {
            for item in items {
                collect_toplevel_fns(item, facts);
            }
        }
        Expression::Module(_, body) => collect_toplevel_fns(body, facts),
        Expression::Statement(inner)
        | Expression::Expr(inner)
        | Expression::ExprStatement(inner)
        | Expression::Group(inner) => collect_toplevel_fns(inner, facts),
        Expression::Function {
            name,
            args,
            body: Some(body),
            ..
        } => {
            insert_facts(facts, name, fn_facts(args, body));
            // Nested fns inside this body still count as top-level for recursion.
            collect_toplevel_fns(body, facts);
        }
        // Skip `impl` methods — see [`analyze_recursive_fns`].
        _ => {}
    }
}

/// Per-function effect flags after call-graph closure (unknown → impure).
pub fn analyze_fn_effects(ast: &Output<'_>) -> HashMap<String, EffectFlags> {
    let facts = collect_fn_facts(ast);
    effect_closure(&facts)
}

/// Names of user functions with no observable side effects.
///
/// Unlike [`analyze_recursive_pure`] this keeps non-recursive functions, so
/// callers that only need "safe to evaluate on another thread" (loop IPA) can
/// admit ordinary helpers such as `fn sq(int i) -> int { i * i }`.
#[cfg(test)]
pub fn analyze_pure_fns(ast: &Output<'_>) -> HashSet<String> {
    analyze_fn_effects(ast)
        .into_iter()
        .filter(|(_, flags)| flags.is_pure())
        .map(|(name, _)| name)
        .collect()
}

/// What the loop length proofs may assume about calls (see
/// `docs/internals/limitations.md`, impure calls in counted loops).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LengthStability {
    /// User functions that cannot change the length of any array, even one
    /// they can reach. Always a superset of the pure set.
    pub fns: HashSet<String>,
    /// No `fn drop()` can change an array length. Finalizers run at
    /// allocation safepoints, so when this is false only pure calls are
    /// length-stable and allocating ops stay barriers.
    pub alloc_stable: bool,
}

/// True when some `fn drop()` in `ast` may change an array's length.
pub fn finalizers_may_resize(ast: &Output<'_>) -> bool {
    finalizers_resize(&analyze_fn_effects(ast))
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

/// Analyze top-level / nested `fn` declarations and return self-recursive pure names.
pub fn analyze_recursive_pure(ast: &Output<'_>) -> RecursivePureSet {
    let facts = collect_fn_facts(ast);
    let impure = impure_closure(&facts);
    facts
        .iter()
        .filter(|(name, f)| !impure.contains(*name) && f.callees.contains(*name))
        .map(|(name, _)| name.clone())
        .collect()
}

/// Fixed point of "impure if locally impure, or any callee is impure / not a
/// user `fn`" over the call graph.
fn impure_closure(facts: &HashMap<String, FnFacts>) -> HashSet<String> {
    effect_closure(facts)
        .into_iter()
        .filter(|(_, flags)| !flags.is_pure())
        .map(|(name, _)| name)
        .collect()
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

/// Effect bits for a host / virtual-module callee (I6), by registry name
/// (`math_sin`, `fs_exists`) or by the short name a virtual module exports
/// (`sin`, `exists`).
///
/// Registry names read the `effects` column of [`common::HOST_NATIVES`]. A
/// short name takes the union of every row it is exported as (`close` is both
/// `io::close` and `thread::close`). Unknown names fail closed
/// (`UNKNOWN | HOST | RESIZE`) so they are never treated as hoistable.
pub fn classify_host_name(name: &str) -> EffectFlags {
    let short = name.rsplit("::").next().unwrap_or(name);
    host_effect_table()
        .get(short)
        .copied()
        .unwrap_or(UNKNOWN_CALLEE)
}

const UNKNOWN_CALLEE: EffectFlags = EffectFlags::from_bits(
    EffectFlags::UNKNOWN | EffectFlags::HOST | EffectFlags::RESIZE,
);

fn host_effect_table() -> &'static HashMap<&'static str, EffectFlags> {
    static TABLE: OnceLock<HashMap<&'static str, EffectFlags>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table: HashMap<&'static str, EffectFlags> = HashMap::new();
        for row in common::HOST_NATIVES {
            table.insert(row.name, row.effects);
        }
        for (_, export) in VirtualModules::new().all_exports() {
            let Some(flags) = export_effects(export) else {
                continue;
            };
            let entry = table.entry(export_surface(export)).or_default();
            *entry = entry.union(flags);
        }
        // Names that are neither a host row nor a virtual export but reach
        // this table unresolved (methods and stdlib helpers called by name).
        for (name, bits) in [
            ("len", EffectFlags::HOST | EffectFlags::UNKNOWN),
            ("write_all", EffectFlags::WRITE),
            ("fd", EffectFlags::ATTACH_PARK | EffectFlags::RESIZE),
        ] {
            table.entry(name).or_insert(EffectFlags::from_bits(bits));
        }
        table
    })
}

fn export_surface(export: &BuiltinExport) -> &'static str {
    match export {
        BuiltinExport::Enum { name }
        | BuiltinExport::TypeClass { name }
        | BuiltinExport::OpaqueType { name } => name,
        BuiltinExport::FfiTag { variant } => variant,
        BuiltinExport::FfiFn { kind } => kind.as_str(),
        BuiltinExport::Fn { kind } => kind.as_str(),
        BuiltinExport::IoFn { kind } => kind.as_str(),
        BuiltinExport::StringFn { kind } => kind.as_str(),
        BuiltinExport::ThreadFn { kind } => kind.as_str(),
        BuiltinExport::GcFn { kind } => kind.as_str(),
        BuiltinExport::HostFn { surface, .. } => surface,
    }
}

/// Effects of calling a virtual-module export; `None` for types.
fn export_effects(export: &BuiltinExport) -> Option<EffectFlags> {
    let row = |registry: &str| {
        common::HOST_NATIVES
            .iter()
            .find(|n| n.name == registry)
            .map(|n| n.effects)
            .unwrap_or(UNKNOWN_CALLEE)
    };
    Some(match export {
        BuiltinExport::Enum { .. }
        | BuiltinExport::TypeClass { .. }
        | BuiltinExport::OpaqueType { .. }
        | BuiltinExport::FfiTag { .. } => return None,
        BuiltinExport::FfiFn { .. } => {
            EffectFlags::from_bits(EffectFlags::FFI | EffectFlags::RESIZE)
        }
        BuiltinExport::Fn { kind } => match kind.math_native_name() {
            Some(registry) => row(registry),
            None if *kind == PreludeFn::Assert => {
                EffectFlags::from_bits(EffectFlags::HOST | EffectFlags::UNKNOWN)
            }
            // `ord`, `char`, `block_on`, matrix helpers: compiled inline or
            // in userland, not a host row.
            None => UNKNOWN_CALLEE,
        },
        BuiltinExport::IoFn { kind } => row(kind.native_name()),
        BuiltinExport::StringFn { kind } => match kind.native_name() {
            Some(registry) => row(registry),
            // `format` lowers to the FORMAT opcode. Kept impure like the
            // other text helpers (see `TEXT` in `common::host`).
            None => EffectFlags::from_bits(EffectFlags::READ),
        },
        BuiltinExport::ThreadFn { kind } => row(kind.native_name()),
        BuiltinExport::GcFn { kind } => row(kind.native_name()),
        BuiltinExport::HostFn { registry, .. } => row(registry),
    })
}

/// `by_owner` keys impl methods `Owner::m` (so method calls can resolve to
/// them); otherwise they share the bare name with any same-named `fn`.
fn collect_fns(ast: &Output<'_>, facts: &mut HashMap<String, FnFacts>, by_owner: bool) {
    match ast.1.as_ref() {
        Expression::Program(items) | Expression::Block(items) | Expression::Fragment(items) => {
            for item in items {
                collect_fns(item, facts, by_owner);
            }
        }
        Expression::Module(_, body) => collect_fns(body, facts, by_owner),
        Expression::Statement(inner)
        | Expression::Expr(inner)
        | Expression::ExprStatement(inner)
        | Expression::Group(inner) => collect_fns(inner, facts, by_owner),
        Expression::Function {
            name,
            args,
            body: Some(body),
            ..
        } => {
            insert_facts(facts, name, fn_facts(args, body));
            collect_nested_fns(body, facts);
        }
        Expression::Implementation { owner, methods, .. } if by_owner => {
            for m in methods {
                if let Some((name, args, body)) = method_fn(m) {
                    insert_facts(facts, &format!("{owner}::{name}"), fn_facts(args, body));
                    collect_nested_fns(body, facts);
                }
            }
        }
        Expression::Implementation { methods, .. } => {
            for m in methods {
                collect_fns(m, facts, by_owner);
            }
        }
        Expression::Method(_, inner) | Expression::Member(inner) => {
            collect_fns(inner, facts, by_owner)
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

fn collect_nested_fns(ast: &Output<'_>, facts: &mut HashMap<String, FnFacts>) {
    match ast.1.as_ref() {
        Expression::Program(items)
        | Expression::Block(items)
        | Expression::Fragment(items)
        | Expression::List(items)
        | Expression::Array(items)
        | Expression::Tuple(items) => {
            for item in items {
                collect_nested_fns(item, facts);
            }
        }
        Expression::Function {
            name,
            args,
            body: Some(body),
            ..
        } => {
            insert_facts(facts, name, fn_facts(args, body));
            collect_nested_fns(body, facts);
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
        | Expression::Member(inner) => collect_nested_fns(inner, facts),
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
            collect_nested_fns(a, facts);
            collect_nested_fns(b, facts);
        }
        Expression::Call { name, args } => {
            collect_nested_fns(name, facts);
            if let Some(args) = args {
                for a in args {
                    collect_nested_fns(a, facts);
                }
            }
        }
        Expression::If(branches) => {
            for b in branches {
                collect_nested_fns(b, facts);
            }
        }
        Expression::Branch(cond, body) => {
            if let Some(c) = cond {
                collect_nested_fns(c, facts);
            }
            collect_nested_fns(body, facts);
        }
        Expression::Match { scrutinee, arms } => {
            collect_nested_fns(scrutinee, facts);
            for arm in arms {
                collect_nested_fns(&arm.body, facts);
            }
        }
        Expression::Construct { fields, .. } => match fields {
            EnumConstructPayload::Tuple(items) => {
                for item in items {
                    collect_nested_fns(item, facts);
                }
            }
            EnumConstructPayload::Record(fields) => {
                for f in fields {
                    collect_nested_fns(&f.value, facts);
                }
            }
            EnumConstructPayload::Unit => {}
        },
        Expression::Loop {
            identifier,
            pattern: _,
            iterable,
            body,
        } => {
            if let Some(id) = identifier {
                collect_nested_fns(id, facts);
            }
            collect_nested_fns(iterable, facts);
            collect_nested_fns(body, facts);
        }
        Expression::Defer { body, .. } | Expression::Lambda { body, .. } => {
            collect_nested_fns(body, facts);
        }
        Expression::Variable(_, Some(init)) => collect_nested_fns(init, facts),
        Expression::Constant(_, Some(init)) => collect_nested_fns(init, facts),
        Expression::LetDestructure { rhs, .. } => collect_nested_fns(rhs, facts),
        Expression::Resume(t, arg) => {
            collect_nested_fns(t, facts);
            if let Some(a) = arg {
                collect_nested_fns(a, facts);
            }
        }
        Expression::Adjust { target, .. } => collect_nested_fns(target, facts),
        Expression::Index(base, Some(idx)) => {
            collect_nested_fns(base, facts);
            collect_nested_fns(idx, facts);
        }
        Expression::Index(base, None) | Expression::Access(base, _) => {
            collect_nested_fns(base, facts);
        }
        Expression::NamedArg(_, v) => collect_nested_fns(v, facts),
        Expression::Implementation { methods, .. } => {
            for m in methods {
                collect_nested_fns(m, facts);
            }
        }
        _ => {}
    }
}

fn walk_body(ast: &Output<'_>, facts: &mut FnFacts) {
    match ast.1.as_ref() {
        Expression::Program(items)
        | Expression::Block(items)
        | Expression::Fragment(items)
        | Expression::List(items)
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
        | Expression::Negate(inner)
        | Expression::Not(inner)
        | Expression::LogicalNot(inner)
        | Expression::Positive(inner)
        | Expression::Cast(inner, _)
        | Expression::TypeOf(inner)
        | Expression::Readonly(inner)
        | Expression::OptionalAccess(inner, _) => walk_body(inner, facts),
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
        | Expression::Range {
            start: a, end: b, ..
        } => {
            walk_body(a, facts);
            walk_body(b, facts);
        }
        Expression::Assignment(lhs, rhs) | Expression::CompoundAssign(lhs, _, rhs) => {
            if writes_shared_heap(lhs, &facts.private) {
                facts.local.insert(EffectFlags::HEAP_MUT);
            }
            walk_body(lhs, facts);
            walk_body(rhs, facts);
        }
        Expression::Adjust { target, .. } => {
            if writes_shared_heap(target, &facts.private) {
                facts.local.insert(EffectFlags::HEAP_MUT);
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
                Expression::Access(recv, member) => {
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
        Expression::Construct { fields, .. } => match fields {
            EnumConstructPayload::Tuple(items) => {
                for item in items {
                    walk_body(item, facts);
                }
            }
            EnumConstructPayload::Record(fields) => {
                for f in fields {
                    walk_body(&f.value, facts);
                }
            }
            EnumConstructPayload::Unit => {}
        },
        Expression::Loop {
            identifier,
            pattern: _,
            iterable,
            body,
        } => {
            if let Some(id) = identifier {
                walk_body(id, facts);
            }
            walk_body(iterable, facts);
            walk_body(body, facts);
        }
        Expression::Variable(_, Some(init)) => walk_body(init, facts),
        Expression::Constant(_, Some(init)) => walk_body(init, facts),
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
fn private_locals(args: &Output<'_>, body: &Output<'_>) -> HashSet<String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    each_binder(args, &mut |n| *counts.entry(n.to_string()).or_default() += 1);
    each_binder(body, &mut |n| *counts.entry(n.to_string()).or_default() += 1);
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
                | Expression::List(_)
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

/// Fill [`Checker::fn_effects`] / [`Checker::pure_fn_names`] after infer.
///
/// Effects are also kept per [`DefId`] for the whole program, so a module
/// checked later sees an imported function's real effects instead of
/// treating the call as unknown (modules are checked in dependency order).
pub fn record_fn_effects(checker: &mut super::infer::Checker, ast: &Output<'_>) {
    checker.fn_effects.clear();
    checker.pure_fn_names.clear();
    let effects = {
        let checker = &*checker;
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
        collect_fns(ast, &mut facts, true);
        let target = |span: (usize, usize), m: &str| {
            let owner = checker.class_owner_at_span(span)?;
            let local = owner
                .strip_prefix(checker.current_module_name())
                .and_then(|o| o.strip_prefix("::"))
                .unwrap_or(&owner);
            Some(format!("{local}::{m}"))
        };
        settle_method_calls(&mut facts, &target);
        effect_closure_with(&facts, &resolve)
    };
    checker.length_stability = length_stability(
        &effects,
        checker.program_finalizers_resize.unwrap_or(false),
    );
    let module = checker.current_module_name().to_string();
    // Impl methods are keyed `Owner::m`. A bare `m` stays listed when every
    // fn or method of that short name is pure, as before methods had owners.
    let mut by_short: HashMap<&str, EffectFlags> = HashMap::new();
    for (name, flags) in &effects {
        let short = name.rsplit_once("::").map_or(name.as_str(), |(_, s)| s);
        let e = by_short.entry(short).or_insert(EffectFlags::empty());
        *e = e.union(*flags);
    }
    for (short, flags) in by_short {
        if flags.is_pure() {
            checker.pure_fn_names.insert(short.to_string());
        }
    }
    for (name, flags) in &effects {
        if name.contains("::") {
            let qualified = if module.is_empty() {
                name.clone()
            } else {
                format!("{module}::{name}")
            };
            if flags.is_pure() {
                checker.pure_fn_names.insert(name.clone());
                checker.pure_fn_names.insert(qualified.clone());
            }
            checker.program_method_effects.insert(qualified, *flags);
            continue;
        }
        if flags.is_pure() {
            checker.pure_fn_names.insert(name.clone());
        }
        if let Some(id) = checker.def_id_of(name) {
            checker.fn_effects.insert(id, *flags);
            checker.program_fn_effects.insert(id, *flags);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parser::Pratt;

    fn pure_set(src: &str) -> RecursivePureSet {
        let owned = src.to_string();
        let ast = Pratt::default().parse(owned.as_str()).expect("parse");
        analyze_recursive_pure(&ast)
    }

    #[test]
    fn fib_is_recursive_pure() {
        let set = pure_set(
            r#"
fn fib(int n) -> int {
    if n <= 1 { return n; }
    return fib(n - 1) + fib(n - 2);
}
fn main() { return; }
"#,
        );
        assert!(set.contains("fib"), "fib should be recursive pure: {set:?}");
        assert!(!set.contains("main"));
    }

    fn parse_ast(src: &str) -> parser::ast::Output<'static> {
        let owned = Box::leak(src.to_string().into_boxed_str());
        Pratt::default().parse(owned).expect("parse")
    }

    #[test]
    fn analyze_recursive_fns_detects_mutual_cycle() {
        let ast = parse_ast(
            r#"
fn ping(int n) -> int { return pong(n); }
fn pong(int n) -> int { return ping(n); }
fn main() { return; }
"#,
        );
        let rec = analyze_recursive_fns(&ast);
        assert!(rec.contains("ping") && rec.contains("pong"), "{rec:?}");
        assert!(!rec.contains("main"));
    }

    #[test]
    fn analyze_recursive_fns_skips_impl_methods() {
        // Identifier `join` inside an impl method must not invent a self-cycle
        // on the method name (see analyze_recursive_fns docs).
        let ast = parse_ast(
            r#"
class T {
    pub x: int,
}
impl T {
    pub fn join() -> int {
        return join(self);
    }
}
fn main() { return; }
"#,
        );
        let rec = analyze_recursive_fns(&ast);
        assert!(
            !rec.contains("join"),
            "impl methods must be excluded from recursion SCC: {rec:?}"
        );
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

    #[test]
    fn pure_non_recursive_excluded() {
        let set = pure_set(
            r#"
fn add(int a, int b) -> int { return a + b; }
fn main() { return; }
"#,
        );
        assert!(!set.contains("add"));
    }

    /// `analyze_pure_fns` keeps the non-recursive helpers that loop IPA needs.
    #[test]
    fn analyze_pure_fns_keeps_non_recursive_helpers() {
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
        let set = analyze_pure_fns(&ast);
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
            "rec must not be recursive-pure when it reaches impure leaf: {set:?}"
        );
    }

    #[test]
    fn index_store_marks_function_impure() {
        let set = pure_set(
            r#"
fn bump([int] a, int n) -> int {
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
        let set = analyze_pure_fns(&ast);
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
        let set = analyze_pure_fns(&ast);
        assert!(
            set.contains("absorb"),
            "counted helper with no host/push must stay pure: {set:?}"
        );
    }

    #[test]
    fn mutual_recursion_without_self_call_excluded() {
        let set = pure_set(
            r#"
fn a(int n) -> int {
    if n <= 0 { return 0; }
    return b(n - 1);
}
fn b(int n) -> int {
    if n <= 0 { return 1; }
    return a(n - 1);
}
fn main() { return; }
"#,
        );
        assert!(
            !set.contains("a") && !set.contains("b"),
            "only self-recursive pure fns are auto-par candidates: {set:?}"
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

    /// Self-calls that only appear inside `Tree::Node(…)` must still mark the
    /// builder recursive-pure so EnumCtor IPA can fire.
    #[test]
    fn self_recursion_only_via_enum_ctor_is_recursive_pure() {
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
            "enum-building self-recursion must be recursive-pure: {set:?}"
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
    fn classify_host_name_math_is_pure_clocks_and_io_are_not() {
        assert!(classify_host_name("sin").is_pure());
        assert!(classify_host_name("math_sin").is_pure());
        assert!(classify_host_name("math::pow").is_pure());
        assert!(classify_host_name("packed_dot").is_pure());
        assert!(classify_host_name("simd_axpy_reduce").is_pure());
        assert!(classify_host_name("mono_nanos").contains(EffectFlags::HOST));
        assert!(classify_host_name("clock_sleep_ms").contains(EffectFlags::HOST));
        assert!(classify_host_name("write").contains(EffectFlags::IO));
        assert!(classify_host_name("gc_collect").contains(EffectFlags::GC));
        assert!(classify_host_name("invoke").contains(EffectFlags::FFI));
        assert!(!classify_host_name("mystery").is_pure());
    }

    #[test]
    fn math_helper_is_pure_clock_helper_is_not() {
        let ast = parse_ast(
            r#"
use clock::{mono_nanos};
fn wave(float x) -> float { return sin(x); }
fn tick() -> int { return mono_nanos(); }
fn main() { return; }
"#,
        );
        let set = analyze_pure_fns(&ast);
        assert!(
            set.contains("wave"),
            "prelude math must not poison purity: {set:?}"
        );
        assert!(!set.contains("tick"), "clocks are observational: {set:?}");
    }

    #[test]
    fn sidecar_records_pure_helper_and_host_callee() {
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
        let side = c.typed_sidecar();
        assert!(side.name_is_pure("add"), "add must stay pure");
        assert!(!side.name_is_pure("shout"), "host write must be impure");
        let add_id = c.def_id_of("add").expect("add DefId");
        let shout_id = c.def_id_of("shout").expect("shout DefId");
        assert!(side.is_pure_def(add_id));
        assert!(!side.is_pure_def(shout_id));
        let shout_fx = side.effects(shout_id).expect("shout effects");
        assert!(
            shout_fx.contains(EffectFlags::IO) || shout_fx.contains(EffectFlags::UNKNOWN),
            "shout should record IO/unknown, got {shout_fx:?}"
        );
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
        let side = c.typed_sidecar();
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
        let side = c.typed_sidecar();
        assert!(side.name_is_pure("Point::norm1"));
        assert!(side.name_is_pure("Point::twice"), "self call resolves");
        assert!(side.name_is_pure("size"), "pure method keeps caller pure");
        assert!(!side.name_is_pure("nudge"), "field write through a param");
        assert!(!side.name_is_pure("loud"), "IO method");
        // Same method name on another class: the receiver type decides.
        assert!(!side.name_is_pure("other"));
        assert!(!side.name_is_pure("norm1"), "bare name is the union");
    }

    #[test]
    fn sidecar_mono_stem_matches_pure_name() {
        let ast = parse_ast("fn sq(int x) -> int { return x * x; } fn main() { return; }");
        let mut c = crate::typechecking::infer::Checker::new();
        let _ = c.check_program(&ast);
        let side = c.typed_sidecar();
        assert!(side.name_is_pure("sq$mono$1$0"));
        assert!(side.name_is_pure("util::sq"));
        assert!(!side.name_is_pure("mod::Type::sq"));
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
        ));
        assert!(!set.contains("speak"), "speak writes under if let: {set:?}");
    }

    fn stability(src: &str) -> LengthStability {
        let ast = parse_ast(src);
        length_stability(&analyze_fn_effects(&ast), false)
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
        assert!(!analyze_pure_fns(&parse_ast(src)).contains("absorb"));
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
    return len(v);
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
    fn every_virtual_export_reaches_a_host_row() {
        // A new export whose registry name has no HOST_NATIVES row would fall
        // back to UNKNOWN: give it a row (with its effects) instead.
        for (module, export) in VirtualModules::new().all_exports() {
            let registry = match export {
                BuiltinExport::IoFn { kind } => Some(kind.native_name()),
                BuiltinExport::StringFn { kind } => kind.native_name(),
                BuiltinExport::ThreadFn { kind } => Some(kind.native_name()),
                BuiltinExport::GcFn { kind } => Some(kind.native_name()),
                BuiltinExport::HostFn { registry, .. } => Some(*registry),
                BuiltinExport::Fn { kind } => kind.math_native_name(),
                _ => None,
            };
            if let Some(registry) = registry {
                assert!(
                    common::HOST_NATIVES.iter().any(|n| n.name == registry),
                    "{module}::{} -> `{registry}` has no host row",
                    export_surface(export)
                );
            }
        }
    }

    #[test]
    fn host_effects_come_from_the_table_not_the_name() {
        // `close` is exported by both `io` and `thread`: union of both rows.
        let close = classify_host_name("close");
        assert!(close.contains(EffectFlags::IO) && close.contains(EffectFlags::THREAD));
        // `io::fs` and `env` short names used to fall through to UNKNOWN.
        assert!(classify_host_name("exists").contains(EffectFlags::IO));
        assert!(!classify_host_name("exists").contains(EffectFlags::UNKNOWN));
        assert!(classify_host_name("var").contains(EffectFlags::ENV));
        assert!(classify_host_name("exec").contains(EffectFlags::EXEC));
        assert!(classify_host_name("connect").contains(EffectFlags::NET));
        assert!(classify_host_name("wait_ready").contains(EffectFlags::SUSPEND));
        assert!(!classify_host_name("wait_ready").contains(EffectFlags::IO));
        assert!(classify_host_name("remove_file").contains(EffectFlags::WRITE));
        assert!(!classify_host_name("exists").contains(EffectFlags::WRITE));
        assert!(classify_host_name("get").contains(EffectFlags::GC));
        // Same row by registry name and by surface name.
        assert_eq!(classify_host_name("fs_exists"), classify_host_name("exists"));
        assert_eq!(classify_host_name("wait_readable"), classify_host_name("await_readable"));
        // An unresolved name is unknown, even if it looks like math.
        assert_eq!(classify_host_name("log"), UNKNOWN_CALLEE);
        assert!(classify_host_name("vec_push_like").contains(EffectFlags::UNKNOWN));
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
            ("param", "fn rec([int] xs, int n) -> int { if n <= 1 { return n; } xs[0] = n; return rec(xs, n - 1) + rec(xs, n - 2); }"),
            ("alias", "fn rec(int n) -> int { if n <= 1 { return n; } let a = [0]; let b = a; b[0] = n; return rec(n - 1) + rec(n - 2); }"),
            ("stored", "fn rec(int n) -> int { if n <= 1 { return n; } let a = [0]; let b = [a]; a[0] = n; return rec(n - 1) + rec(n - 2) + b[0][0]; }"),
            ("nested", "fn rec([int] xs, int n) -> int { if n <= 1 { return n; } let a = [xs]; a[0][0] = n; return rec(xs, n - 1) + rec(xs, n - 2); }"),
            ("shadowed", "fn rec([int] a, int n) -> int { if n <= 1 { return n; } if n > 5 { let a = [0]; a[0] = 1; } a[0] = n; return rec(a, n - 1) + rec(a, n - 2); }"),
            ("reassigned", "fn rec([int] xs, int n) -> int { if n <= 1 { return n; } let a = [0]; a = xs; a[0] = n; return rec(xs, n - 1) + rec(xs, n - 2); }"),
        ] {
            let set = pure_set(&format!("{body}\nfn main() {{ return; }}\n"));
            assert!(!set.contains("rec"), "{why}: {set:?}");
        }
    }
}
