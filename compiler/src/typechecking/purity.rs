//! Whole-function purity / effects for auto-par, LICM, and the typed sidecar.
//!
//! A function is **pure** when its body has no observable host side effects
//! (IO / threads / FFI / yield / attach) and only calls other pure user
//! functions. Unknown callees are conservatively impure. **Recursive pure**
//! functions may be auto-parallelized at `f(a) ⊕ f(b)` sites.

use std::collections::{HashMap, HashSet};

use parser::ast::{EnumConstructPayload, Expression, Output};

/// Names of user functions that are pure and self-recursive.
pub type RecursivePureSet = HashSet<String>;

/// Observable effects that kill purity. Empty flags are pure.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct EffectFlags(u16);

impl EffectFlags {
    pub const HOST: u16 = 1 << 0;
    pub const FFI: u16 = 1 << 1;
    pub const HEAP_MUT: u16 = 1 << 2;
    pub const YIELD: u16 = 1 << 3;
    pub const THREAD: u16 = 1 << 4;
    pub const GC: u16 = 1 << 5;
    pub const IO: u16 = 1 << 6;
    pub const ATTACH_PARK: u16 = 1 << 7;
    pub const UNKNOWN: u16 = 1 << 8;

    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn from_bits(bits: u16) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u16 {
        self.0
    }

    pub const fn is_pure(self) -> bool {
        self.0 == 0
    }

    pub const fn contains(self, bit: u16) -> bool {
        self.0 & bit != 0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub fn insert(&mut self, bit: u16) {
        self.0 |= bit;
    }
}

#[derive(Default)]
struct FnFacts {
    local: EffectFlags,
    /// Callee names (unqualified Identifier call targets).
    callees: HashSet<String>,
}

/// Collect per-function callee sets (and local impurity) for call-graph analyses.
fn collect_fn_facts(ast: &Output<'_>) -> HashMap<String, FnFacts> {
    let mut facts: HashMap<String, FnFacts> = HashMap::new();
    collect_fns(ast, &mut facts);
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
            body: Some(body),
            ..
        } => {
            let mut f = FnFacts::default();
            walk_body(body, &mut f);
            facts.insert((*name).to_string(), f);
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
pub fn analyze_pure_fns(ast: &Output<'_>) -> HashSet<String> {
    analyze_fn_effects(ast)
        .into_iter()
        .filter(|(_, flags)| flags.is_pure())
        .map(|(name, _)| name)
        .collect()
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
    let user_fns: HashSet<&String> = facts.keys().collect();
    let mut out: HashMap<String, EffectFlags> = HashMap::new();
    for (name, f) in facts {
        let mut flags = f.local;
        for c in &f.callees {
            if !user_fns.contains(c) {
                flags = flags.union(classify_unknown_callee(c));
            }
        }
        out.insert(name.clone(), flags);
    }
    let mut changed = true;
    while changed {
        changed = false;
        for (name, f) in facts {
            let mut flags = out[name];
            for c in &f.callees {
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
    let short = name.rsplit("::").next().unwrap_or(name);
    let mut flags = EffectFlags::empty();
    match short {
        "attach" | "park" => flags.insert(EffectFlags::ATTACH_PARK),
        "spawn" | "join" | "detach" | "channel" | "send" | "recv" | "try_send" | "try_recv"
        | "close" | "mutex" | "with_lock" | "lock" | "try_lock" | "unlock" | "rwlock"
        | "with_read" | "with_write" | "try_read" | "try_write" => {
            flags.insert(EffectFlags::THREAD)
        }
        "root" | "unroot" | "weak" | "upgrade" | "heap_bytes" | "collect"
        | "register_finalizer" => flags.insert(EffectFlags::GC),
        "dload" | "declare" | "invoke" => flags.insert(EffectFlags::FFI),
        "stdin" | "stdout" | "stderr" | "open" | "read" | "write" | "write_from" | "write_all"
        | "await_readable" | "await_writable" | "drive" | "wait_ready" | "from_bytes"
        | "to_bytes" | "connect" | "connect_timeout" | "listen" | "accept" | "peer_addr"
        | "local_addr" | "set_nodelay" | "shutdown" | "bind" | "send_to" | "recv_from"
        | "local_port" | "format" => flags.insert(EffectFlags::IO),
        _ => flags.insert(EffectFlags::UNKNOWN | EffectFlags::HOST),
    }
    flags
}

fn collect_fns(ast: &Output<'_>, facts: &mut HashMap<String, FnFacts>) {
    match ast.1.as_ref() {
        Expression::Program(items) | Expression::Block(items) | Expression::Fragment(items) => {
            for item in items {
                collect_fns(item, facts);
            }
        }
        Expression::Module(_, body) => collect_fns(body, facts),
        Expression::Statement(inner)
        | Expression::Expr(inner)
        | Expression::ExprStatement(inner)
        | Expression::Group(inner) => collect_fns(inner, facts),
        Expression::Function {
            name,
            body: Some(body),
            ..
        } => {
            let mut f = FnFacts::default();
            walk_body(body, &mut f);
            facts.insert((*name).to_string(), f);
            collect_nested_fns(body, facts);
        }
        Expression::Implementation { methods, .. } => {
            for m in methods {
                collect_fns(m, facts);
            }
        }
        Expression::Method(_, inner) | Expression::Member(inner) => collect_fns(inner, facts),
        _ => {}
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
            body: Some(body),
            ..
        } => {
            let mut f = FnFacts::default();
            walk_body(body, &mut f);
            facts.insert((*name).to_string(), f);
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
        Expression::Yield(_) | Expression::YieldFrom(_) | Expression::Resume(_, _) => {
            facts.local.insert(EffectFlags::YIELD);
        }
        Expression::Declare(_) | Expression::Invoke(_) => {
            facts.local.insert(EffectFlags::FFI);
        }
        Expression::Panic(_) | Expression::Defer { .. } => {
            facts.local.insert(EffectFlags::UNKNOWN);
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
            if matches!(
                peel(lhs).1.as_ref(),
                Expression::Index(_, _) | Expression::Access(_, _)
            ) {
                facts.local.insert(EffectFlags::HEAP_MUT);
            }
            walk_body(lhs, facts);
            walk_body(rhs, facts);
        }
        Expression::Adjust { target, .. } => {
            if matches!(
                peel(target).1.as_ref(),
                Expression::Index(_, _) | Expression::Access(_, _)
            ) {
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
                _ => {
                    facts.local.insert(EffectFlags::UNKNOWN);
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
        _ => {}
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
pub fn record_fn_effects(checker: &mut super::infer::Checker, ast: &Output<'_>) {
    checker.fn_effects.clear();
    checker.pure_fn_names.clear();
    let effects = analyze_fn_effects(ast);
    for (name, flags) in &effects {
        if flags.is_pure() {
            checker.pure_fn_names.insert(name.clone());
        }
        if let Some(id) = checker.def_id_of(name) {
            checker.fn_effects.insert(id, *flags);
        }
    }
    debug_assert_eq!(checker.pure_fn_names, analyze_pure_fns(ast));
}

#[cfg(test)]
mod tests {
    use super::*;
    use parser::Pratt;

    fn pure_set(src: &str) -> RecursivePureSet {
        let owned = src.to_string();
        let ast = Pratt::default()
            .parse(owned.as_str())
            .expect("parse");
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
fn bump(int n) -> int {
    let a = [0];
    a[0] = n;
    if n <= 1 { return a[0]; }
    return bump(n - 1) + bump(n - 2);
}
fn main() { return; }
"#,
        );
        assert!(
            !set.contains("bump"),
            "index assignment is a side effect: {set:?}"
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
    fn sidecar_mono_stem_matches_pure_name() {
        let ast = parse_ast("fn sq(int x) -> int { return x * x; } fn main() { return; }");
        let mut c = crate::typechecking::infer::Checker::new();
        let _ = c.check_program(&ast);
        let side = c.typed_sidecar();
        assert!(side.name_is_pure("sq$mono$1$0"));
        assert!(side.name_is_pure("util::sq"));
        assert!(!side.name_is_pure("mod::Type::sq"));
    }
}
