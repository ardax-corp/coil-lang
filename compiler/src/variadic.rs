//! Instances of variadic `... xs` functions (#1281).
//!
//! The checker types each call of a template `fn f(… , ... xs)` as a call to
//! `f`'s copy for that call's pack, and lists the packs that have no copy yet
//! (`typechecking::infer` `variadic`). For each one, this module appends a
//! clone of `f` named `f__vaN` to the module; the module is checked again
//! until no call asks for a new copy. Calls are renamed in place to their
//! copy and the templates dropped before a last check, so codegen sees
//! ordinary functions.
//!
//! A clone's spans move to a region of their own ([`SPAN_BASE`] + N ×
//! [`SPAN_REGION`]): span-keyed checker facts of two copies never collide,
//! and [`original_span`] maps a diagnostic back to the template's source.

use std::ops::Range;

use parser::SimpleSpan;
use parser::ast::{Expression, MatchArm, Output, Pattern, PatternPayload};
use reporting::Message;

use crate::attrs::leak;
use crate::typechecking::{Checker, Ty};

/// Start of the first copy's span region.
pub const SPAN_BASE: usize = 0x6000_0000;
/// Bytes of source each copy's region holds.
pub const SPAN_REGION: usize = 0x0100_0000;
/// Copies one module may make (keeps every region inside `u32`).
pub const MAX_INSTANCES: usize = 64;
/// Check rounds before the instance list is final.
const MAX_ROUNDS: usize = 32;

/// A copy: which template, for which pack.
#[derive(Clone, Debug)]
pub struct Instance {
    pub name: String,
    pub template: String,
    pub pack: Ty,
}

/// Check `ast`, adding instances until every template call has one. Returns
/// the instances made, in region order; their names are already in the AST.
pub fn check_with_instances<'a>(checker: &mut Checker, ast: &mut Output<'a>) -> Vec<Instance> {
    checker.variadic_begin_module();
    // Each round re-checks from the state before the module's first check,
    // so its classes, enums and overloads are not declared twice.
    let snapshot = has_templates(ast).then(|| checker.clone());
    let mut instances: Vec<Instance> = Vec::new();
    // Rounds that added copies; a round that only renamed calls re-checks
    // once more and asks for nothing new.
    let mut rounds = 0;
    let mut first = true;
    loop {
        if !first && let Some(snapshot) = &snapshot {
            *checker = snapshot.clone();
            for inst in &instances {
                checker.add_variadic_instance(&inst.template, inst.pack.clone(), &inst.name);
            }
        }
        first = false;
        let last_chance = instances.len() >= MAX_INSTANCES || rounds >= MAX_ROUNDS;
        checker.set_variadic_exhausted(last_chance);
        let _ = checker.check_program(ast);
        let pending = checker.take_variadic_pending();
        let renamed = rename_calls(ast, checker);
        let templates = checker.has_variadic_templates();
        if pending.is_empty() && renamed == 0 && !templates {
            break;
        }
        if !pending.is_empty() && last_chance {
            // The calls without a copy keep their errors.
            break;
        }
        // This round's messages are stale: the next round re-checks it all.
        let _ = checker.take_messages();
        if pending.is_empty() {
            // Every call names its copy now: check once more without the
            // templates (codegen compiles only what the last check saw).
            drop_templates(ast, checker);
            continue;
        }
        for (template, pack) in pending {
            if instances.len() >= MAX_INSTANCES {
                break;
            }
            let index = instances.len();
            let name = format!("{template}__va{index}");
            if add_instance(ast, &template, &name, SPAN_BASE + index * SPAN_REGION) {
                checker.add_variadic_instance(&template, pack.clone(), &name);
                instances.push(Instance { name, template, pack });
            }
        }
        rounds += 1;
    }
    instances
}

/// The source span `span` was cloned from, and which copy holds it.
pub fn original_span(span: Range<usize>) -> Option<(usize, Range<usize>)> {
    if span.start < SPAN_BASE {
        return None;
    }
    let index = (span.start - SPAN_BASE) / SPAN_REGION;
    let base = SPAN_BASE + index * SPAN_REGION;
    let end = span.end.saturating_sub(base).max(span.start - base);
    Some((index, span.start - base..end))
}

/// Point a diagnostic inside a copy at the template's source, naming the
/// argument types it was compiled for, and spell copies by their template.
pub fn remap_message(msg: &Message, instances: &[Instance]) -> Message {
    let mut text = msg.message().to_string();
    for inst in instances {
        text = text.replace(&inst.name, &inst.template);
    }
    let Some((index, range)) = original_span(msg.range().clone()) else {
        if text == msg.message() {
            return msg.clone();
        }
        let mut out = Message::new(*msg.kind(), text, msg.range().clone());
        if let Some(code) = msg.code() {
            out.set_code(code);
        }
        if let Some(h) = msg.help() {
            out.with_help(h.to_string());
        }
        return out;
    };
    let mut out = Message::new(*msg.kind(), text, range);
    if let Some(code) = msg.code() {
        out.set_code(code);
    }
    let mut help = match instances.get(index) {
        Some(inst) => format!("in `{}` called with `{}`", inst.template, inst.pack),
        None => String::new(),
    };
    if let Some(h) = msg.help() {
        help = if help.is_empty() { h.to_string() } else { format!("{h}\n{help}") };
    }
    if !help.is_empty() {
        out.with_help(help);
    }
    out
}

/// Append a clone of top-level `fn template` named `name`, its spans moved
/// by `base`. False when the module has no such function.
fn add_instance<'a>(ast: &mut Output<'a>, template: &str, name: &str, base: usize) -> bool {
    let Expression::Program(children) = ast.1.as_mut() else {
        return false;
    };
    let Some(found) = children
        .iter()
        .find(|c| matches!(c.1.as_ref(), Expression::Function { name, .. } if *name == template))
    else {
        return false;
    };
    let mut copy = found.clone();
    respan(&mut copy, base);
    if let Expression::Function { name: fn_name, docs, .. } = copy.1.as_mut() {
        *fn_name = leak(name.to_string());
        docs.clear();
    }
    children.push(copy);
    true
}

/// Whether the module declares a free `fn` with an untyped `... xs`.
fn has_templates(ast: &Output<'_>) -> bool {
    let Expression::Program(children) = ast.1.as_ref() else {
        return false;
    };
    children.iter().any(|c| match c.1.as_ref() {
        Expression::Function { args, .. } => matches!(args.1.as_ref(), Expression::Fragment(items)
            if items.last().is_some_and(|a| {
                matches!(a.1.as_ref(), Expression::Argument { ty: None, is_rest: true, .. })
            })),
        _ => false,
    })
}

/// Rename each call the checker resolved to a copy. Returns how many.
fn rename_calls(ast: &mut Output<'_>, checker: &Checker) -> usize {
    let mut count = 0;
    rename_in(ast, checker, &mut count);
    count
}

fn rename_in(node: &mut Output<'_>, checker: &Checker, count: &mut usize) {
    let span = node.0;
    if let Expression::Call { name, .. } = node.1.as_mut()
        && let Expression::Identifier(callee) = name.1.as_mut()
        && checker.is_variadic_template(callee)
        && let Some(instance) = checker.variadic_call_at(span.start, span.end)
    {
        *callee = leak(instance.to_string());
        *count += 1;
    }
    node.1.for_each_child_mut(&mut |c| rename_in(c, checker, count));
}

/// Replace each template with a no-op: only its copies are compiled.
fn drop_templates(ast: &mut Output<'_>, checker: &Checker) {
    let Expression::Program(children) = ast.1.as_mut() else {
        return;
    };
    for child in children.iter_mut() {
        if let Expression::Function { name, .. } = child.1.as_ref()
            && checker.is_variadic_template(name)
        {
            let span = child.0;
            *child.1 = Expression::Noop((span, Box::new(Expression::Bool(true))));
        }
    }
}

fn shift(span: SimpleSpan, base: usize) -> SimpleSpan {
    SimpleSpan::from(span.start + base..span.end + base)
}

/// Move every span under `node` (patterns and contracts too) by `base`.
fn respan(node: &mut Output<'_>, base: usize) {
    node.0 = shift(node.0, base);
    match node.1.as_mut() {
        Expression::Function { contracts, effects, .. } => {
            for c in contracts {
                c.span = shift(c.span, base);
                respan(&mut c.expr, base);
            }
            if let Some(e) = effects {
                e.span = shift(e.span, base);
            }
        }
        Expression::Loop { contracts, .. } => {
            for c in contracts {
                c.span = shift(c.span, base);
                respan(&mut c.expr, base);
            }
        }
        Expression::Match { arms, .. } => arms.iter_mut().for_each(|a| respan_arm(a, base)),
        Expression::IfLet { then_arm, else_arm, .. } => {
            respan_arm(then_arm, base);
            respan_arm(else_arm, base);
        }
        Expression::WhileLet { then_arm, on_miss, .. } => {
            respan_arm(then_arm, base);
            respan_arm(on_miss, base);
        }
        _ => {}
    }
    node.1.for_each_child_mut(&mut |c| respan(c, base));
}

fn respan_arm(arm: &mut MatchArm<'_>, base: usize) {
    arm.pattern.0 = shift(arm.pattern.0, base);
    respan_pattern(&mut arm.pattern.1, base);
}

fn respan_pattern(pattern: &mut Pattern<'_>, base: usize) {
    if let Pattern::Constructor { payload, .. } = pattern {
        match payload {
            PatternPayload::Unit => {}
            PatternPayload::Tuple(items) => {
                for (span, p) in items {
                    *span = shift(*span, base);
                    respan_pattern(p, base);
                }
            }
            PatternPayload::Record(fields) => {
                for f in fields {
                    f.pattern.0 = shift(f.pattern.0, base);
                    respan_pattern(&mut f.pattern.1, base);
                }
            }
        }
    }
}
