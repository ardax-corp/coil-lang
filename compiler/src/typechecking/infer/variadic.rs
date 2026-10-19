//! Variadic `... xs` functions (#1281).
//!
//! A free `fn f(…, ... xs)` whose rest parameter has no type is a template:
//! each call packs its own trailing arguments into a tuple, and the body is
//! checked and compiled once per distinct pack type, as an instance with the
//! pack fixed (`xs[0]`, `len(xs)` and `xs[i]` then type like on any tuple).
//!
//! The template's own body is never checked. A call whose pack has no
//! instance yet asks for one ([`VariadicState::pending`]); the module is then
//! re-checked with that instance added as a top-level `fn` (see
//! `crate::variadic`), until every call has its instance.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use parser::ast::{Expression, Output};
use reporting::ErrorCode;

use crate::typechecking::id::NodeId;
use crate::typechecking::subst::apply_ty_prune;
use crate::typechecking::ty::{Ty, ftv_ty};

use super::Checker;

/// Template / instance bookkeeping of the module being checked.
#[derive(Clone, Debug, Default)]
pub struct VariadicState {
    /// Templates declared in this module (instances excluded).
    templates: HashSet<String>,
    /// `(template, pack)` → instance name, across re-check rounds.
    instances: HashMap<(String, Ty), String>,
    /// Instance name → `(template, pack)`.
    instance_of: HashMap<String, (String, Ty)>,
    /// Packs this round's calls need that have no instance yet.
    pending: Vec<(String, Ty)>,
    /// Call span → the instance it calls.
    calls: HashMap<(usize, usize), String>,
    /// No more instances will be made: a missing one is final.
    exhausted: bool,
}

/// Templates other modules may call, and the copies made of them (#1386).
/// Kept across modules: an importer is checked after the template's module.
#[derive(Clone, Debug, Default)]
pub struct VariadicExports {
    /// `module::template` → its parameter names (the last is the pack).
    templates: HashMap<String, Vec<String>>,
    /// `(module::template, pack)` → `module::copy`.
    copies: HashMap<(String, Ty), String>,
    /// Copies this round's calls need: `(module::template, pack, call span)`.
    pending: Vec<(String, Ty, Range<usize>)>,
}

/// The tuple-rest parameter of a function's argument list, if it has one.
pub fn has_tuple_rest(args: &Output) -> bool {
    matches!(args.1.as_ref(), Expression::Fragment(children)
        if children.last().is_some_and(|c| {
            matches!(c.1.as_ref(), Expression::Argument { ty: None, is_rest: true, .. })
        }))
}

impl Checker {
    /// Forget every template and instance: a new module starts.
    pub fn variadic_begin_module(&mut self) {
        self.variadic = VariadicState::default();
    }

    /// Record `instance` as `template`'s copy for `pack`.
    pub fn add_variadic_instance(&mut self, template: &str, pack: Ty, instance: &str) {
        self.variadic
            .instances
            .insert((template.to_string(), pack.clone()), instance.to_string());
        self.variadic
            .instance_of
            .insert(instance.to_string(), (template.to_string(), pack));
    }

    /// Packs the last check asked for, each once.
    pub fn take_variadic_pending(&mut self) -> Vec<(String, Ty)> {
        std::mem::take(&mut self.variadic.pending)
    }

    /// Mark the instance list final (no more re-check rounds).
    pub fn set_variadic_exhausted(&mut self, exhausted: bool) {
        self.variadic.exhausted = exhausted;
    }

    pub fn has_variadic_templates(&self) -> bool {
        !self.variadic.templates.is_empty()
    }

    pub fn is_variadic_template(&self, name: &str) -> bool {
        self.variadic.templates.contains(name)
    }

    /// `(template, pack)` of an instance.
    pub fn variadic_instance_of(&self, name: &str) -> Option<&(String, Ty)> {
        self.variadic.instance_of.get(name)
    }

    /// Instance a call (by span) resolves to.
    pub fn variadic_call_at(&self, start: usize, end: usize) -> Option<&str> {
        self.variadic.calls.get(&(start, end)).map(String::as_str)
    }

    /// Whether any module exported a template.
    pub fn has_variadic_exports(&self) -> bool {
        !self.variadic_exports.templates.is_empty()
    }

    /// Let importers call `module::template`, declared as `function` (#1386).
    pub fn export_variadic_template(&mut self, fqn: &str, function: &Output) {
        if let Expression::Function { args, .. } = function.1.as_ref() {
            self.variadic_exports
                .templates
                .insert(fqn.to_string(), Self::syntactic_param_names(args));
        }
    }

    /// Record `module::copy` as the copy of `module::template` for `pack`.
    pub fn add_variadic_export_copy(&mut self, template: &str, pack: Ty, copy: &str) {
        self.variadic_exports
            .copies
            .insert((template.to_string(), pack), copy.to_string());
    }

    /// How many copies importers can call (names of new ones count up).
    pub fn variadic_export_copies(&self) -> usize {
        self.variadic_exports.copies.len()
    }

    /// Whether a call naming `ident` may resolve to a copy.
    pub fn is_variadic_call_target(&self, ident: &str) -> bool {
        self.is_variadic_template(ident) || self.foreign_variadic_template(ident).is_some()
    }

    /// Copies of other modules' templates the last check asked for.
    pub fn take_variadic_foreign_pending(&mut self) -> Vec<(String, Ty, Range<usize>)> {
        std::mem::take(&mut self.variadic_exports.pending)
    }

    /// Check `ast` (a module's imports plus `copy`, a copy of one of its
    /// templates) as module `module`, with `copy` an instance for `pack`.
    /// The importer's variadic state is kept. Returns the copies the calls
    /// in `ast` resolved to, by call span.
    pub fn check_variadic_copy(
        &mut self,
        module: &str,
        ast: &Output,
        copy: &str,
        template: &str,
        pack: Ty,
    ) -> HashMap<(usize, usize), String> {
        let importer = std::mem::replace(&mut self.current_module, module.to_string());
        let state = std::mem::take(&mut self.variadic);
        self.add_variadic_instance(template, pack, copy);
        let _ = self.check_program(ast);
        let calls = std::mem::replace(&mut self.variadic, state).calls;
        self.current_module = importer;
        calls
    }

    /// `module::template` when `ident` names another module's template: an
    /// import of it, its qualified name, or (inside one of its copies) the
    /// template's own bare name.
    pub fn foreign_variadic_template(&self, ident: &str) -> Option<String> {
        if self.variadic_exports.templates.is_empty() {
            return None;
        }
        let candidates = [
            Some(ident.to_string()),
            self.import_locals.get(ident).cloned(),
            (!self.current_module.is_empty()).then(|| format!("{}::{ident}", self.current_module)),
        ];
        candidates
            .into_iter()
            .flatten()
            .find(|fqn| fqn.contains("::") && self.variadic_exports.templates.contains_key(fqn))
    }

    /// A call to another module's template `fqn` (as `ident` here): type it
    /// as a call to that module's copy for its pack, or ask for one.
    pub(super) fn infer_foreign_variadic_call(
        &mut self,
        ident: &str,
        fqn: &str,
        raw_args: &[Output],
        id: Option<NodeId>,
        range: Range<usize>,
    ) -> Ty {
        let names = self.variadic_exports.templates[fqn].clone();
        let fixed = names.len().saturating_sub(1);
        // Pack the trailing arguments like a call in the template's module.
        self.fn_param_names.insert(ident.to_string(), names);
        self.fn_has_rest.insert(ident.to_string(), true);
        self.fn_tuple_rest.insert(ident.to_string(), true);
        let (arg_tys, ordered_args) = self.infer_and_reorder_call_args(ident, raw_args, &range);
        let pack = match self.variadic_call_pack(ident, &arg_tys, fixed, &range) {
            Ok(pack) => pack,
            Err(ty) => return ty,
        };
        let key = (fqn.to_string(), pack.clone());
        let Some(instance) = self.variadic_exports.copies.get(&key).cloned() else {
            if !self.variadic_exports.pending.iter().any(|(t, p, _)| *t == key.0 && *p == key.1) {
                self.variadic_exports.pending.push((key.0, key.1, range.clone()));
            }
            return self.missing_variadic_copy(ident, &pack, range);
        };
        self.variadic.calls.insert((range.start, range.end), instance.clone());
        self.apply_variadic_instance(ident, &instance, &arg_tys, &ordered_args, id, range)
    }

    /// Per-round reset plus template discovery, before the pre-passes.
    pub(super) fn collect_variadic_templates(&mut self, ast: &Output) {
        self.variadic.templates.clear();
        self.variadic.pending.clear();
        self.variadic.calls.clear();
        // Other modules' copies, called by their qualified names (#1386).
        for ((template, _), copy) in &self.variadic_exports.copies {
            if let Some(names) = self.variadic_exports.templates.get(template) {
                self.fn_param_names.insert(copy.clone(), names.clone());
                self.fn_has_rest.insert(copy.clone(), true);
                self.fn_tuple_rest.insert(copy.clone(), true);
            }
        }
        let Expression::Program(children) = ast.1.as_ref() else {
            return;
        };
        for child in children {
            let Expression::Function { name, args, .. } = child.1.as_ref() else {
                continue;
            };
            if !has_tuple_rest(args) {
                continue;
            }
            self.fn_has_rest.insert(name.to_string(), true);
            self.fn_tuple_rest.insert(name.to_string(), true);
            if !self.variadic.instance_of.contains_key(*name) {
                self.variadic.templates.insert(name.to_string());
                if !self.current_module.is_empty() {
                    let fqn = format!("{}::{name}", self.current_module);
                    self.variadic_exports
                        .templates
                        .insert(fqn, Self::syntactic_param_names(args));
                }
            }
        }
    }

    /// The pack type an instance's `... xs` has.
    pub(super) fn variadic_instance_pack(&self, name: &str) -> Option<Ty> {
        self.variadic.instance_of.get(name).map(|(_, pack)| pack.clone())
    }

    /// A call to template `ident`: type it as a call to the instance for its
    /// pack, or ask for that instance.
    pub(super) fn infer_variadic_call(
        &mut self,
        ident: &str,
        raw_args: &[Output],
        id: Option<NodeId>,
        range: Range<usize>,
    ) -> Ty {
        let (arg_tys, ordered_args) = self.infer_and_reorder_call_args(ident, raw_args, &range);
        let fixed = self.fn_param_names.get(ident).map_or(0, |n| n.len().saturating_sub(1));
        let pack = match self.variadic_call_pack(ident, &arg_tys, fixed, &range) {
            Ok(pack) => pack,
            Err(ty) => return ty,
        };
        let key = (ident.to_string(), pack.clone());
        let Some(instance) = self.variadic.instances.get(&key).cloned() else {
            if !self.variadic.pending.contains(&key) {
                self.variadic.pending.push(key);
            }
            return self.missing_variadic_copy(ident, &pack, range);
        };
        self.variadic.calls.insert((range.start, range.end), instance.clone());
        self.apply_variadic_instance(ident, &instance, &arg_tys, &ordered_args, id, range)
    }

    /// The pack of a template call whose arguments typed as `arg_tys`.
    fn variadic_call_pack(
        &mut self,
        ident: &str,
        arg_tys: &[Ty],
        fixed: usize,
        range: &Range<usize>,
    ) -> Result<Ty, Ty> {
        let pack = match arg_tys.last() {
            Some(t) if arg_tys.len() == fixed + 1 => apply_ty_prune(&self.subst, t),
            _ => {
                return Err(self.error_with_help(
                    ErrorCode::GenericTypeError,
                    format!("`{ident}` packs its trailing arguments, so a call must pass its {fixed} fixed ones"),
                    range.clone(),
                    Some("a function with a `... xs` parameter cannot be partially applied".to_string()),
                ));
            }
        };
        if !ftv_ty(&pack).is_empty() {
            return Err(self.error_with_help(
                ErrorCode::GenericTypeError,
                format!("cannot tell the types of the arguments `{ident}` packs into its `...` parameter"),
                range.clone(),
                Some("give each packed argument a known type (annotate the `let` it comes from)".to_string()),
            ));
        }
        Ok(pack)
    }

    /// The error a call without a copy for its pack reports this round.
    fn missing_variadic_copy(&mut self, ident: &str, pack: &Ty, range: Range<usize>) -> Ty {
        let message = if self.variadic.exhausted {
            format!(
                "`{ident}` is called with too many different argument lists (no copy made for `{}`)",
                self.diag_ty(pack)
            )
        } else {
            format!("no copy of `{ident}` for `{}` yet", self.diag_ty(pack))
        };
        self.error_with_help(
            ErrorCode::GenericTypeError,
            message,
            range,
            Some(format!(
                "each argument list compiles `{ident}` once; a call inside it whose list grows on every call never ends"
            )),
        )
    }

    /// Type a template call as a call to `instance`.
    fn apply_variadic_instance(
        &mut self,
        ident: &str,
        instance: &str,
        arg_tys: &[Ty],
        ordered_args: &[Output],
        id: Option<NodeId>,
        range: Range<usize>,
    ) -> Ty {
        let Some(scheme) = self.lookup_fn_scheme(instance) else {
            return self.error(
                ErrorCode::UnknownFunction,
                format!("Cannot find function `{ident}`"),
                range,
            );
        };
        let (fun_ty, constraints, mapping) = self.instantiate_scheme_mapped(&scheme);
        let result = self.apply_function(
            Some(instance),
            &fun_ty,
            arg_tys,
            if ordered_args.is_empty() { None } else { Some(ordered_args) },
            id,
            range.clone(),
        );
        if !constraints.is_empty() {
            self.bind_call_result_to_expected(&result);
            self.discharge_constraints(id, &constraints, &range);
            self.pin_assoc_after_discharge("", &constraints, Some(&scheme), &mapping, &range);
        }
        result
    }
}
