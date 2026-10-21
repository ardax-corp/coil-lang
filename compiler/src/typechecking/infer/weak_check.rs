//! `gc::weak` on readonly class instances (#1644, #1670).
//!
//! A weak handle observes an instance's identity and the moment it is
//! collected, so a readonly class instance (a value, #1562) may never be
//! weakened. A direct `weak(p)` is refused at the call. Through generics the
//! argument is a type parameter, so the restriction becomes a requirement on
//! that parameter: every instantiation that sets it to a readonly class is
//! refused at the instantiating call, and an instantiation that sets it to
//! another type parameter carries the requirement on to that parameter's own
//! callers (a generic calling a generic that calls `weak`).
//!
//! Everything is resolved once substitution is closed, since a forward call
//! instantiates a generic before its body (and its `weak` calls) is checked.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use reporting::{ErrorCode, Label, Message};

use crate::typechecking::subst::apply_ty_prune;
use crate::typechecking::ty::{Scheme, Ty, TyVarId};

use super::Checker;

/// Weak-argument checks of the module being checked, plus the requirements
/// found so far (kept across modules: a later module instantiates an earlier
/// one's generics).
#[derive(Clone, Default)]
pub(in crate::typechecking::infer) struct WeakChecks {
    /// Type variables that must not be a readonly class: a `weak` call, maybe
    /// through other generics, takes a value of that type.
    required: HashMap<TyVarId, Vec<Requirement>>,
    /// A forward stub's type parameter → the parameter of the body it stands for.
    aliases: HashMap<TyVarId, TyVarId>,
    /// Enclosing functions of the recorded sites; `stack` indexes into it.
    fns: Vec<FnCtx>,
    stack: Vec<usize>,
    args: Vec<WeakArg>,
    insts: Vec<Inst>,
    /// The expression being inferred, which a scheme instantiation is for.
    pub(super) range: Range<usize>,
}

#[derive(Clone)]
struct Requirement {
    /// Set for a parameter of an enclosing scope (an `impl`'s `T`): only the
    /// instantiations of this one function's scheme carry the requirement,
    /// not every method of the impl.
    scheme_ty: Option<Ty>,
    /// The `weak` call the requirement comes from, and its module.
    origin: Range<usize>,
    module: String,
}

#[derive(Clone)]
struct FnCtx {
    own_params: Vec<TyVarId>,
    fun_ty: Ty,
    /// The function this one is declared in.
    parent: Option<usize>,
}

#[derive(Clone)]
struct WeakArg {
    ty: Ty,
    range: Range<usize>,
    ctx: Option<usize>,
}

#[derive(Clone)]
struct Inst {
    mapping: Vec<(TyVarId, TyVarId)>,
    /// The instantiated scheme's type; `None` for a generic trait instance
    /// (`impl Show for Box<T>`), whose dictionary holds all its methods.
    scheme_ty: Option<Ty>,
    range: Range<usize>,
    ctx: Option<usize>,
}

impl WeakChecks {
    /// Drop the per-module records; requirements and aliases stay.
    pub(super) fn clear_module(&mut self) {
        self.fns.clear();
        self.stack.clear();
        self.args.clear();
        self.insts.clear();
    }

    pub(super) fn enter_fn(&mut self, own_params: Vec<TyVarId>, fun_ty: Ty) {
        let parent = self.stack.last().copied();
        self.fns.push(FnCtx {
            own_params,
            fun_ty,
            parent,
        });
        self.stack.push(self.fns.len() - 1);
    }

    pub(super) fn leave_fn(&mut self) {
        self.stack.pop();
    }

    /// `stub`'s parameters (a forward stub's scheme) are `params`.
    pub(super) fn alias(&mut self, stub: &[TyVarId], params: &[TyVarId]) {
        for (&s, &p) in stub.iter().zip(params) {
            if s != p {
                self.aliases.insert(s, p);
            }
        }
    }

    pub(super) fn record_arg(&mut self, ty: Ty, range: Range<usize>) {
        let ctx = self.stack.last().copied();
        self.args.push(WeakArg { ty, range, ctx });
    }

    pub(super) fn record_inst(&mut self, scheme: &Scheme, mapping: impl IntoIterator<Item = (TyVarId, TyVarId)>) {
        if scheme.bounds.is_empty() {
            return;
        }
        let mapping: Vec<(TyVarId, TyVarId)> = mapping.into_iter().collect();
        self.insts.push(Inst {
            mapping,
            scheme_ty: Some(scheme.ty.clone()),
            range: self.range.clone(),
            ctx: self.stack.last().copied(),
        });
    }

    /// A generic trait instance selected for a goal at `range`.
    pub(super) fn record_instance(&mut self, mapping: Vec<(TyVarId, TyVarId)>, range: &Range<usize>) {
        if mapping.is_empty() {
            return;
        }
        self.insts.push(Inst {
            mapping,
            scheme_ty: None,
            range: range.clone(),
            ctx: self.stack.last().copied(),
        });
    }
}

impl Checker {
    fn weak_root(&self, var: TyVarId) -> TyVarId {
        let var = self.weak_checks.aliases.get(&var).copied().unwrap_or(var);
        match apply_ty_prune(&self.subst, &Ty::Var(var)) {
            Ty::Var(root) => self.weak_checks.aliases.get(&root).copied().unwrap_or(root),
            _ => var,
        }
    }

    /// Require `var`, seen in the function `ctx`, not to be a readonly class.
    /// Returns whether this is new.
    fn require_weakable(&mut self, var: TyVarId, ctx: Option<usize>, origin: &Range<usize>, module: &str) -> bool {
        let var = self.weak_root(var);
        // A function's own parameter is its alone; any other (an impl's
        // `T`) is shared by the impl's methods, so tie the requirement to the
        // outermost enclosing function's scheme.
        let mut scheme_ty = None;
        let mut at = ctx;
        while let Some(c) = at {
            let f = &self.weak_checks.fns[c];
            if f.own_params.contains(&var) {
                scheme_ty = None;
                break;
            }
            scheme_ty = Some(apply_ty_prune(&self.subst, &f.fun_ty));
            at = f.parent;
        }
        let reqs = self.weak_checks.required.entry(var).or_default();
        if reqs.iter().any(|r| r.scheme_ty == scheme_ty) {
            return false;
        }
        reqs.push(Requirement {
            scheme_ty,
            origin: origin.clone(),
            module: module.to_string(),
        });
        true
    }

    /// E0119 for each `gc::weak` argument that resolved to a readonly class
    /// instance (#1644), directly or through the type parameters of the
    /// generics it was passed through (#1670). Runs once substitution is
    /// closed.
    pub(super) fn run_pending_weak_checks(&mut self) {
        let module = self.current_module.clone();
        let args = std::mem::take(&mut self.weak_checks.args);
        for arg in args {
            let resolved = apply_ty_prune(&self.subst, &arg.ty);
            if let Ty::Var(v) = resolved {
                self.require_weakable(v, arg.ctx, &arg.range, &module);
                continue;
            }
            if !self.is_readonly_class_ty(&resolved) {
                continue;
            }
            let shown = self.diag_ty(&resolved);
            self.error_with_help(
                ErrorCode::GenericTypeError,
                format!("`weak()` is not allowed on `{shown}`, an instance of a readonly class"),
                arg.range,
                Some(format!(
                    "a readonly class instance is a value, with no identity or lifetime to observe; keep the `{shown}` itself, or make it a plain `class` if you need a weak handle"
                )),
            );
        }

        let insts = std::mem::take(&mut self.weak_checks.insts);
        let mut reported: HashSet<(usize, usize)> = HashSet::new();
        let mut changed = true;
        while changed {
            changed = false;
            for inst in &insts {
                for &(bound, fresh) in &inst.mapping {
                    let Some(reqs) = self.weak_checks.required.get(&self.weak_root(bound)).cloned() else {
                        continue;
                    };
                    for req in reqs {
                        if let (Some(want), Some(have)) = (&req.scheme_ty, &inst.scheme_ty)
                            && apply_ty_prune(&self.subst, have) != *want
                        {
                            continue;
                        }
                        let resolved = apply_ty_prune(&self.subst, &Ty::Var(fresh));
                        if let Ty::Var(v) = resolved {
                            changed |= self.require_weakable(v, inst.ctx, &req.origin, &req.module);
                        } else if self.is_readonly_class_ty(&resolved)
                            && reported.insert((inst.range.start, inst.range.end))
                        {
                            self.report_weak_instantiation(&resolved, inst.range.clone(), &req, &module);
                        }
                    }
                }
            }
        }
    }

    fn report_weak_instantiation(&mut self, ty: &Ty, range: Range<usize>, req: &Requirement, module: &str) {
        let shown = self.diag_ty(ty);
        let mut msg = Message::error(
            ErrorCode::GenericTypeError,
            format!(
                "`weak()` is not allowed on `{shown}`, an instance of a readonly class: this call passes it to a generic that calls `weak()` on it"
            ),
            range.clone(),
        );
        msg.push(Label::new(format!("this call sets a type parameter to `{shown}`"), range));
        if req.module == module {
            msg.push(Label::new(
                "`weak()` is called on it here".to_string(),
                req.origin.clone(),
            ));
        }
        msg.with_help(format!(
            "a readonly class instance is a value, with no identity or lifetime to observe; keep the `{shown}` out of this generic, or make it a plain `class` if you need a weak handle"
        ));
        self.messages.push(msg);
    }
}
