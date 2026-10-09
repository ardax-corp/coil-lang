//! Pipeline stage that turns the entry file's contracts into test cases
//! (`coil test`, contracts step C3). See `docs/internals/contracts.md`.
//!
//! For every function of the entry file with a `requires` or `ensures` whose
//! parameter types all have an `Arbitrary` instance, a `test("contract: f")`
//! case draws arguments with `arbitrary::any`, skips draws that break a
//! `requires`, and calls the function in a child task (`arbitrary::run_case`)
//! so a failed `ensures` (or any other panic) fails the case with the
//! arguments that caused it. Codegen drops the cases of functions with
//! effects beyond reads and mutation (`Compiler::contract_cases`): random
//! arguments must not write files or open sockets.

use std::collections::{HashMap, HashSet};

use parser::ast::{ContractKind, Expression, Output};

use super::Pipeline;
use super::macros::GeneratedRange;

/// Primitive types with an instance in the `arbitrary` module.
const PRIMITIVES: &[&str] = &["int", "byte", "bool", "float", "string"];

/// One function to test: how to call it, its parameters (name and type) and
/// its `requires` clauses as written.
struct Target {
    /// `f` or `Owner::f`.
    path: String,
    params: Vec<(String, String, Option<String>)>,
    requires: Vec<String>,
    /// The function's header: where the case's diagnostics point.
    site: std::ops::Range<usize>,
}

impl Pipeline {
    /// Add contract test cases to the entry file (when `coil test` asked for
    /// them with `set_contract_runs`).
    pub(super) fn add_contract_tests(&mut self) {
        if self.contract_runs == 0 || !self.include_tests || self.contracts() == crate::ContractLevel::Off {
            return;
        }
        let Some(entry) = self.entry_file.clone() else { return };
        let files: Vec<_> = self.processed.clone();
        let mut arbitrary: HashSet<String> = PRIMITIVES.iter().map(|p| p.to_string()).collect();
        let mut shown: HashSet<String> = HashSet::new();
        for file in &files {
            if let Some(ast) = self.ast_cache.get(file).and_then(|c| c.ast()) {
                collect_instances(ast, &mut arbitrary, &mut shown);
            }
        }
        let Some(cached) = self.ast_cache.get_mut(&entry) else { return };
        let Some(ast) = cached.ast() else { return };
        let targets = targets(ast, &arbitrary, &shown);
        if targets.is_empty() {
            return;
        }
        let import = "use arbitrary::{Gen as __PropGen, any as __prop_any, run_case as __prop_run, \
                      quote as __prop_quote, show_vec as __prop_show_vec, show_option as __prop_show_option};";
        let mut items = Vec::new();
        let mut ranges = Vec::new();
        let mut messages = Vec::new();
        for (i, t) in std::iter::once(None).chain(targets.iter().map(Some)).enumerate() {
            let text = match t {
                None => import.to_string(),
                Some(t) => case_text(t, self.contract_runs),
            };
            let site = t.map_or(0..1, |t| t.site.clone());
            match cached.parse_generated(&text) {
                Ok((generated, range)) => {
                    if let Expression::Program(mut parsed) = *generated.1 {
                        items.append(&mut parsed);
                    }
                    ranges.push(GeneratedRange {
                        file: entry.clone(),
                        range,
                        site,
                        origin: "the contract tests of this file".to_string(),
                        text,
                    });
                }
                Err(err) => {
                    let mut msg = reporting::Message::error(
                        reporting::ErrorCode::GenericTypeError,
                        format!("internal: a generated contract test does not parse: {}", err.message()),
                        site,
                    );
                    msg.with_help(format!("generated code:\n{text}"));
                    messages.push(msg);
                    if i == 0 {
                        break;
                    }
                }
            }
        }
        cached.push_expand_messages(messages);
        if items.is_empty() {
            return;
        }
        let Some(Expression::Program(children)) = cached.ast_mut().map(|a| a.1.as_mut()) else { return };
        // The `use` first, with the file's own imports; the cases at the end.
        let mut items = items.into_iter();
        if let Some(import) = items.next() {
            children.insert(0, import);
        }
        children.extend(items);
        self.generated_ranges.extend(ranges);
        let cases: HashMap<String, String> = targets.iter().map(|t| (case_name(t), t.path.clone())).collect();
        self.compiler_lazy_mut().set_contract_cases(cases);
        // The `use arbitrary::…` above.
        self.discover_all();
    }
}

fn case_name(t: &Target) -> String {
    format!("contract: {}", t.path)
}

/// Types with an `Arbitrary` instance and types with a `Show` instance, by
/// head name (`Point`, `Pair`).
fn collect_instances(ast: &Output<'_>, arbitrary: &mut HashSet<String>, shown: &mut HashSet<String>) {
    let Expression::Program(items) = ast.1.as_ref() else { return };
    for item in items {
        let Expression::TypeClassImpl { class, args, .. } = item.1.as_ref() else { continue };
        let trait_name = class.rsplit("::").next().unwrap_or(class);
        let Some(head) = args.first().and_then(|a| type_head(a)) else { continue };
        match trait_name {
            "Arbitrary" => {
                arbitrary.insert(head.to_string());
            }
            "Show" => {
                shown.insert(head.to_string());
            }
            _ => {}
        }
    }
}

fn type_head<'e>(ty: &Output<'e>) -> Option<&'e str> {
    match ty.1.as_ref() {
        Expression::Type(n) => Some(n),
        Expression::TypeApp { name, .. } => Some(name),
        _ => None,
    }
}

/// True when `any` can make a value of `ty`.
fn generatable(ty: &Output<'_>, arbitrary: &HashSet<String>) -> bool {
    match ty.1.as_ref() {
        Expression::Type(n) => arbitrary.contains(*n),
        Expression::TypeApp { name, args } => {
            (matches!(*name, "Vec" | "Option") || arbitrary.contains(*name))
                && args.iter().all(|a| generatable(a, arbitrary))
        }
        _ => false,
    }
}

/// An expression showing local `name` of type `ty`, or `None` for `<type>`.
fn show_expr(ty: &Output<'_>, name: &str, shown: &HashSet<String>) -> Option<String> {
    let simple = |t: &Output<'_>| match t.1.as_ref() {
        Expression::Type(n) => *n != "string" && (PRIMITIVES.contains(n) || shown.contains(*n)),
        _ => false,
    };
    match ty.1.as_ref() {
        Expression::Type("string") => Some(format!("__prop_quote({name})")),
        Expression::Type(n) if PRIMITIVES.contains(n) || shown.contains(*n) => Some(format!("({name}).show()")),
        Expression::TypeApp { name: "Vec", args } if args.len() == 1 && simple(&args[0]) => {
            Some(format!("__prop_show_vec({name})"))
        }
        Expression::TypeApp { name: "Option", args } if args.len() == 1 && simple(&args[0]) => {
            Some(format!("__prop_show_option({name})"))
        }
        _ => None,
    }
}

/// Functions and static methods of the entry file with contracts and
/// generatable parameters.
fn targets(ast: &Output<'_>, arbitrary: &HashSet<String>, shown: &HashSet<String>) -> Vec<Target> {
    let Expression::Program(items) = ast.1.as_ref() else { return Vec::new() };
    let mut out = Vec::new();
    for item in items {
        match item.1.as_ref() {
            Expression::Implementation { owner, type_params, methods, .. } if type_params.is_empty() => {
                for m in methods {
                    if let Some(t) = target(m, Some(owner), arbitrary, shown) {
                        out.push(t);
                    }
                }
            }
            _ => {
                if let Some(t) = target(item, None, arbitrary, shown) {
                    out.push(t);
                }
            }
        }
    }
    out
}

fn target(node: &Output<'_>, owner: Option<&str>, arbitrary: &HashSet<String>, shown: &HashSet<String>) -> Option<Target> {
    let (public, f) = match node.1.as_ref() {
        Expression::Method(v, inner) => (*v == parser::ast::Visibility::Public, inner.1.as_ref()),
        other => (false, other),
    };
    let Expression::Function { name, is_coro, is_static, type_params, args, contracts, body, .. } = f else {
        return None;
    };
    let has_clauses = contracts
        .iter()
        .any(|c| matches!(c.kind, ContractKind::Requires | ContractKind::Ensures));
    if !has_clauses || *is_coro || !type_params.is_empty() || body.is_none() || name.starts_with("__") {
        return None;
    }
    // Instance methods need a receiver that keeps the class invariant, and
    // the case calls from outside the impl.
    if owner.is_some() && (!*is_static || !public) {
        return None;
    }
    let Expression::Fragment(arg_nodes) = args.1.as_ref() else { return None };
    let mut params = Vec::new();
    for a in arg_nodes {
        let Expression::Argument { name, ty: Some(ty), is_rest: false, .. } = a.1.as_ref() else {
            return None;
        };
        if !generatable(ty, arbitrary) {
            return None;
        }
        params.push((name.to_string(), ty.1.to_string(), show_expr(ty, name, shown)));
    }
    let requires = contracts
        .iter()
        .filter(|c| c.kind == ContractKind::Requires)
        .map(|c| c.text.to_string())
        .collect();
    let path = match owner {
        Some(o) => format!("{o}::{name}"),
        None => name.to_string(),
    };
    let start = node.0.start;
    let end = body.as_ref().map_or(node.0.end, |b| b.0.start).max(start + 1);
    Some(Target { path, params, requires, site: start..end })
}

/// FNV-1a of the case name, kept in 31 bits and away from 0: each function
/// draws the same arguments on every run.
fn seed_of(name: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in name.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    (h & 0x7fff_ffff).max(1)
}

/// The `test("contract: f") { … }` source for `t`.
fn case_text(t: &Target, runs: u32) -> String {
    let name = case_name(t);
    let mut draws = String::new();
    for (p, ty, _) in &t.params {
        draws.push_str(&format!("        let {p}: {ty} = __prop_any(__prop_g);\n"));
    }
    let mut checks = String::new();
    for r in &t.requires {
        checks.push_str(&format!("        if !({r}) {{\n            continue;\n        }}\n"));
    }
    let names: Vec<&str> = t.params.iter().map(|(p, _, _)| p.as_str()).collect();
    let capture = if names.is_empty() { String::new() } else { format!(" use ({})", names.join(", ")) };
    let shown: Vec<String> = t
        .params
        .iter()
        .map(|(p, ty, show)| match show {
            Some(e) => format!("\"{p} = \" + {e}"),
            None => format!("\"{p} = <{ty}>\""),
        })
        .collect();
    let shown = if shown.is_empty() { "\"\"".to_string() } else { shown.join(" + \", \" + ") };
    // No parameters: one call is enough.
    let runs = if t.params.is_empty() { 1 } else { runs };
    format!(
        "test({name:?}) {{\n    \
         let __prop_g = __PropGen::new({seed});\n    \
         let __prop_ran = 0;\n    \
         let __prop_tries = 0;\n    \
         while __prop_ran < {runs} && __prop_tries < {tries} {{\n        \
         __prop_g.resize(__prop_ran + (__prop_tries - __prop_ran) / 10);\n        \
         __prop_tries += 1;\n\
         {draws}{checks}        \
         __prop_ran += 1;\n        \
         let __prop_failed = __prop_run(fn (){capture} {{\n            {path}({args});\n        }});\n        \
         match __prop_failed {{\n            \
         Option::Some(prop_message__) => {{\n                \
         raise \"{path}(\" + {shown} + \"): \" + prop_message__;\n            }},\n            \
         Option::None => {{}},\n        }}\n    }}\n}}\n",
        seed = seed_of(&name),
        tries = runs * 10,
        path = t.path,
        args = names.join(", "),
    )
}
