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

use std::collections::HashMap;
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
/// Start of the region of the first copy made for another module (#1386).
pub const FOREIGN_SPAN_BASE: usize = SPAN_BASE + MAX_INSTANCES * SPAN_REGION;
/// Check rounds before the instance list is final.
const MAX_ROUNDS: usize = 32;

/// A copy: which template, for which pack.
#[derive(Clone, Debug)]
pub struct Instance {
    pub name: String,
    pub template: String,
    pub pack: Ty,
}

/// A template of an already-checked module, which importers may need copies
/// of (#1386).
#[derive(Clone)]
pub struct ForeignTemplate<'a> {
    /// The declaring module.
    pub module: String,
    /// The template's name there.
    pub name: String,
    /// The template's `fn` as declared.
    pub function: Output<'a>,
    /// The declaring module's `use` items, so the copy resolves like it.
    pub uses: Vec<Output<'a>>,
}

impl ForeignTemplate<'_> {
    pub fn fqn(&self) -> String {
        format!("{}::{}", self.module, self.name)
    }
}

/// A copy of another module's template made for this module's calls: a
/// program of that module's `use` items and the copy, compiled in that
/// module's namespace.
pub struct ForeignCopy<'a> {
    pub module: String,
    /// The copy's name in `module`.
    pub name: String,
    /// The template's name in `module`.
    pub template: String,
    pub pack: Ty,
    /// The first call that asked for the copy (where its errors point).
    pub call: Range<usize>,
    pub ast: Output<'a>,
}

impl ForeignCopy<'_> {
    pub fn fqn(&self) -> String {
        format!("{}::{}", self.module, self.name)
    }

    fn template_fqn(&self) -> String {
        format!("{}::{}", self.module, self.template)
    }
}

/// What [`check_with_instances`] made.
#[derive(Default)]
pub struct Checked<'a> {
    /// This module's instances, in region order; their names are already in
    /// the AST.
    pub instances: Vec<Instance>,
    /// Copies made in other modules, in region order from
    /// [`FOREIGN_SPAN_BASE`]. Each must be compiled in its module.
    pub copies: Vec<ForeignCopy<'a>>,
    /// This module's templates, as declared (the AST holds no-ops now).
    pub templates: Vec<Output<'a>>,
}

/// Check `ast`, adding instances until every template call has one; calls
/// to `foreign` templates get copies made in the template's module.
pub fn check_with_instances<'a>(
    checker: &mut Checker,
    ast: &mut Output<'a>,
    foreign: &[ForeignTemplate<'a>],
) -> Checked<'a> {
    checker.variadic_begin_module();
    let _ = checker.take_variadic_foreign_pending();
    // Each round re-checks from the state before the module's first check,
    // so its classes, enums and overloads are not declared twice.
    let snapshot = (has_templates(ast) || checker.has_variadic_exports()).then(|| checker.clone());
    let mut instances: Vec<Instance> = Vec::new();
    let mut copies: Vec<ForeignCopy<'a>> = Vec::new();
    let mut templates: Vec<Output<'a>> = Vec::new();
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
            // Copies in other modules: declare them again before the calls.
            for copy in &copies {
                checker.add_variadic_export_copy(&copy.template_fqn(), copy.pack.clone(), &copy.fqn());
            }
            for copy in &mut copies {
                let calls = checker.check_variadic_copy(
                    &copy.module,
                    &copy.ast,
                    &copy.name,
                    &copy.template,
                    copy.pack.clone(),
                );
                rename_by(&mut copy.ast, &calls);
            }
        }
        first = false;
        let made = instances.len() + copies.len();
        let last_chance = made >= MAX_INSTANCES || rounds >= MAX_ROUNDS;
        checker.set_variadic_exhausted(last_chance);
        let _ = checker.check_program(ast);
        let pending = checker.take_variadic_pending();
        let foreign_pending = checker.take_variadic_foreign_pending();
        let renamed = rename_calls(ast, checker);
        let has_templates = checker.has_variadic_templates();
        let asks = !pending.is_empty() || !foreign_pending.is_empty();
        if !asks && renamed == 0 && !has_templates {
            break;
        }
        if asks && last_chance {
            // The calls without a copy keep their errors.
            break;
        }
        // This round's messages are stale: the next round re-checks it all.
        let _ = checker.take_messages();
        if !asks {
            // Every call names its copy now: check once more without the
            // templates (codegen compiles only what the last check saw).
            templates.extend(drop_templates(ast, checker));
            continue;
        }
        for (fqn, pack, call) in foreign_pending {
            if instances.len() + copies.len() >= MAX_INSTANCES {
                break;
            }
            let Some(template) = foreign.iter().find(|t| t.fqn() == fqn) else {
                continue;
            };
            let name = format!("{}__vx{}", template.name, checker.variadic_export_copies());
            let base = FOREIGN_SPAN_BASE + copies.len() * SPAN_REGION;
            let mut function = template.function.clone();
            respan(&mut function, base);
            if let Expression::Function { name: fn_name, docs, .. } = function.1.as_mut() {
                *fn_name = leak(name.clone());
                docs.clear();
            }
            let mut items = template.uses.clone();
            items.push(function);
            let span = SimpleSpan::from(base..base);
            let copy = ForeignCopy {
                module: template.module.clone(),
                name,
                template: template.name.clone(),
                pack,
                call,
                ast: (span, Box::new(Expression::Program(items))),
            };
            checker.add_variadic_export_copy(&fqn, copy.pack.clone(), &copy.fqn());
            copies.push(copy);
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
    // A module's templates and own copies serve its importers too.
    if !checker.current_module_name().is_empty() {
        let module = checker.current_module_name().to_string();
        for function in &templates {
            if let Expression::Function { name, .. } = function.1.as_ref() {
                checker.export_variadic_template(&format!("{module}::{name}"), function);
            }
        }
        for inst in &instances {
            checker.add_variadic_export_copy(
                &format!("{module}::{}", inst.template),
                inst.pack.clone(),
                &format!("{module}::{}", inst.name),
            );
        }
    }
    Checked { instances, copies, templates }
}

/// The source span `span` was cloned from, and which copy holds it.
pub fn original_span(span: Range<usize>) -> Option<(usize, Range<usize>)> {
    if span.start < SPAN_BASE || span.start >= FOREIGN_SPAN_BASE {
        return None;
    }
    let index = (span.start - SPAN_BASE) / SPAN_REGION;
    let base = SPAN_BASE + index * SPAN_REGION;
    let end = span.end.saturating_sub(base).max(span.start - base);
    Some((index, span.start - base..end))
}

/// Point a diagnostic inside a copy at the template's source, naming the
/// argument types it was compiled for, and spell copies by their template.
pub fn remap_message(msg: &Message, instances: &[Instance], copies: &[ForeignCopy<'_>]) -> Message {
    let mut text = msg.message().to_string();
    for inst in instances {
        text = text.replace(&inst.name, &inst.template);
    }
    for copy in copies {
        text = text.replace(&copy.fqn(), &copy.template_fqn());
        text = text.replace(&copy.name, &copy.template);
    }
    // An error inside another module's copy points at the call it serves.
    if msg.range().start >= FOREIGN_SPAN_BASE {
        let index = (msg.range().start - FOREIGN_SPAN_BASE) / SPAN_REGION;
        let mut out = Message::new(
            *msg.kind(),
            text,
            copies.get(index).map_or(0..0, |c| c.call.clone()),
        );
        if let Some(code) = msg.code() {
            out.set_code(code);
        }
        let mut help = copies
            .get(index)
            .map(|c| format!("in `{}` called with `{}`", c.template_fqn(), diag_pack(&c.pack)))
            .unwrap_or_default();
        if let Some(h) = msg.help() {
            help = if help.is_empty() { h.to_string() } else { format!("{h}\n{help}") };
        }
        if !help.is_empty() {
            out.with_help(help);
        }
        return out;
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
        Some(inst) => format!("in `{}` called with `{}`", inst.template, diag_pack(&inst.pack)),
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
/// A copy's pack as a diagnostic shows it (never a raw `t61`).
fn diag_pack(pack: &Ty) -> String {
    crate::format_ty_for_diag(&Default::default(), pack)
}

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

/// Rename the calls of a copy checked on its own (`calls` by span).
fn rename_by(node: &mut Output<'_>, calls: &HashMap<(usize, usize), String>) {
    let span = node.0;
    if let Expression::Call { name, .. } = node.1.as_mut()
        && let Expression::Identifier(callee) = name.1.as_mut()
        && let Some(instance) = calls.get(&(span.start, span.end))
    {
        *callee = leak(instance.to_string());
    }
    node.1.for_each_child_mut(&mut |c| rename_by(c, calls));
}

fn rename_in(node: &mut Output<'_>, checker: &Checker, count: &mut usize) {
    let span = node.0;
    if let Expression::Call { name, .. } = node.1.as_mut()
        && let Expression::Identifier(callee) = name.1.as_mut()
        && checker.is_variadic_call_target(callee)
        && let Some(instance) = checker.variadic_call_at(span.start, span.end)
    {
        *callee = leak(instance.to_string());
        *count += 1;
    }
    // `module::f(…)` of another module's template (#1386).
    if let Expression::Call { name, .. } = node.1.as_mut()
        && matches!(name.1.as_ref(), Expression::QualifiedAccess { .. })
        && let Some(instance) = checker.variadic_call_at(span.start, span.end)
    {
        *name.1 = Expression::Identifier(leak(instance.to_string()));
        *count += 1;
    }
    node.1.for_each_child_mut(&mut |c| rename_in(c, checker, count));
}

/// Replace each template with a no-op (only its copies are compiled), and
/// return them.
fn drop_templates<'a>(ast: &mut Output<'a>, checker: &Checker) -> Vec<Output<'a>> {
    let Expression::Program(children) = ast.1.as_mut() else {
        return Vec::new();
    };
    let mut dropped = Vec::new();
    for child in children.iter_mut() {
        if let Expression::Function { name, .. } = child.1.as_ref()
            && checker.is_variadic_template(name)
        {
            let span = child.0;
            let noop = Box::new(Expression::Noop((span, Box::new(Expression::Bool(true)))));
            dropped.push((span, std::mem::replace(&mut child.1, noop)));
        }
    }
    dropped
}

/// What a copy of one of `module`'s templates needs in scope: the module's
/// `use` items, plus a `use` of each item it declares (a copy checked on its
/// own sees them as imports). `templates` are left out.
pub fn copy_scope<'a>(module: &str, ast: &Output<'a>, templates: &[&str]) -> Vec<Output<'a>> {
    let Expression::Program(children) = ast.1.as_ref() else {
        return Vec::new();
    };
    let mut out: Vec<Output<'a>> = children
        .iter()
        .filter(|c| matches!(c.1.as_ref(), Expression::Use { .. }))
        .cloned()
        .collect();
    let path: Vec<String> = module.split("::").map(str::to_string).collect();
    let mut seen: Vec<&str> = Vec::new();
    for child in children {
        let name = match child.1.as_ref() {
            Expression::Function { name, .. }
            | Expression::Class { name, .. }
            | Expression::EnumDecl { name, .. }
            | Expression::TypeAlias { name, .. }
            | Expression::StaticDecl { name, .. }
            | Expression::TypeClass { name, .. } => *name,
            _ => continue,
        };
        if templates.contains(&name) || seen.contains(&name) {
            continue;
        }
        seen.push(name);
        let item = Expression::Use { path: path.clone(), name: name.to_string(), alias: None };
        out.push((child.0, Box::new(item)));
    }
    out
}

/// The name a template `fn` declares.
pub fn template_name<'a>(function: &Output<'a>) -> Option<&'a str> {
    match function.1.as_ref() {
        Expression::Function { name, .. } => Some(name),
        _ => None,
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
