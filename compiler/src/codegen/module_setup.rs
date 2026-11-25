//! Module setup as a function of its own (`__module_setup`): the static
//! initializers' stores and each `extern` library's load and declarations.
//! The finalizer registry is one more (`__finalizer_setup`), called first of
//! all. Startup calls them before `main`, so each is a body like any other
//! (`docs/internals/native.md`, "Module setup").
//!
//! The initializers computed while compiling (`const`s and pure `static
//! let`s, [`ComptimeConst`]) are startup steps of their own, ahead of that
//! call: `Pipeline::compute_consts` replaces them with the values' snapshot,
//! and one that cannot be computed keeps its step.

use super::*;
use crate::mir::{MirAllocKind, MirBuilder, MirConst, MirFfiOp, MirFunc, MirGcKind, MirTy};

/// The setup function's name.
pub(crate) const MODULE_SETUP_FN: &str = "__module_setup";

/// The finalizer registry's function name.
const FINALIZER_SETUP_FN: &str = "__finalizer_setup";

/// A body startup calls, and the static its word goes to.
pub(crate) struct StartupStep {
    pub(crate) body: String,
    pub(crate) slot: Option<u32>,
}

/// One step of module setup, in the order its ops run.
pub(crate) enum SetupStep {
    /// `static_slot = init()`; `comptime` when the value is computed while
    /// compiling (it stays in the prologue).
    /// A two-word static's second word goes to `tail`.
    Static { init: String, slot: u32, tail: Option<u32>, comptime: bool },
    /// Load the library named by string `name`; its handle to `slot`.
    Load { name: u32, slot: u32 },
    /// Declare symbol `name` of the library in `lib_slot` with argument
    /// type words `args` and return type word `ret` (`DeclareFFI`'s
    /// `operand`); its function id to `slot` and its C address to
    /// `addr_slot` (an extern call's direct call).
    Declare { lib_slot: u32, name: u32, args: Vec<u32>, ret: u32, operand: u32, slot: u32, addr_slot: u32 },
}

impl Compiler {
    /// Emit `__module_setup` from the setup collected while compiling and
    /// list what runs before `main`, by name: the finalizer registry first,
    /// as a collection while an initializer runs must find the `drop` of
    /// what it made, then the initializers computed while compiling, then
    /// `__module_setup`.
    pub(super) fn emit_module_setup(&mut self) {
        let finalizers = self.finalizer_registry();
        let steps = std::mem::take(&mut self.setup_steps);
        let mut startup = Vec::new();
        let mut setup = Vec::new();
        for step in steps {
            match step {
                SetupStep::Static { ref init, slot, comptime, .. } => {
                    if !self.functions.contains_key(init) {
                        continue;
                    }
                    if comptime {
                        startup.push(StartupStep { body: init.clone(), slot: Some(slot) });
                    } else {
                        setup.push(step);
                    }
                }
                step => setup.push(step),
            }
        }
        // Statics run first, then the `extern` setup, as their ops do.
        setup.sort_by_key(|s| !matches!(s, SetupStep::Static { .. }));
        if !setup.is_empty() {
            let name = MODULE_SETUP_FN.to_string();
            let (_, label) = self.bind_function_entry(name.clone());
            // Its code is emitted from the MIR below.
            self.bodies.advance();
            self.bodies.record(name.clone(), Some(label));
            match self.module_setup_mir(&setup) {
                Ok(mut func) => {
                    func.shrink_to_fit();
                    self.bodies.set_last_mir(func);
                }
                Err(why) => self.bodies.set_last_mir_why(why),
            }
            startup.push(StartupStep { body: name, slot: None });
        }
        if !finalizers.is_empty() {
            self.emit_finalizer_setup(&finalizers);
            startup.insert(0, StartupStep { body: FINALIZER_SETUP_FN.to_string(), slot: None });
        }
        self.startup_steps = startup;
    }

    /// Emit `__finalizer_setup`, the registry of `finalizers`.
    fn emit_finalizer_setup(&mut self, finalizers: &[(usize, u32, Option<crate::bodies::Label>)]) {
        let name = FINALIZER_SETUP_FN.to_string();
        let (_, label) = self.bind_function_entry(name.clone());
        // Its code is emitted from the MIR below.
        self.bodies.advance();
        self.bodies.record(name, Some(label));
        match Self::finalizer_setup_mir(finalizers) {
            Ok(mut func) => {
                func.shrink_to_fit();
                self.bodies.set_last_mir(func);
            }
            Err(why) => self.bodies.set_last_mir_why(why),
        }
    }

    /// `__finalizer_setup`'s MIR.
    fn finalizer_setup_mir(finalizers: &[(usize, u32, Option<crate::bodies::Label>)]) -> Result<MirFunc, String> {
        let mir = |e: crate::mir::MirError| e.to_string();
        let mut b = MirBuilder::new(FINALIZER_SETUP_FN);
        b.allow_host_edges = true;
        for &(native, type_id, drop) in finalizers {
            let type_id = b.ins_const(MirConst::I64(i64::from(type_id))).map_err(mir)?;
            // A cyclic class without a `drop` registers no entry.
            let drop = match drop {
                Some(drop) => b.ins_code_ptr(drop).map_err(mir)?,
                None => b.ins_const(MirConst::I64(0)).map_err(mir)?,
            };
            b.ins_host_invoke(native as u16, vec![type_id, drop]).map_err(mir)?;
        }
        let zero = b.ins_const(MirConst::I64(0)).map_err(mir)?;
        b.ret(Some(zero)).map_err(mir)?;
        b.finish().map_err(mir)
    }

    /// Keep the initializer of static `fqn` in the prologue: its value is
    /// computed while compiling.
    pub(super) fn mark_comptime_setup(&mut self, fqn: &str) {
        let init = format!("{STATIC_INIT_FN_PREFIX}{fqn}");
        if let Some(SetupStep::Static { comptime, .. }) =
            self.setup_steps.iter_mut().rev().find(|s| matches!(s, SetupStep::Static { init: i, .. } if *i == init))
        {
            *comptime = true;
        }
    }

    /// The finalizer registry: `(gc_register_finalizer, type id, drop
    /// entry)` for each class with a `drop`, and for each class whose
    /// objects can close a reference cycle ([`Self::cyclic_classes`],
    /// `CYCLIC_TYPE_FLAG`, no entry without a `drop`), in type-id order
    /// (archives stay reproducible).
    fn finalizer_registry(&self) -> Vec<(usize, u32, Option<crate::bodies::Label>)> {
        let Some(native) = self.native_id("gc_register_finalizer") else {
            return Vec::new();
        };
        let cyclic = self.cyclic_classes();
        let mut owners: Vec<String> = self.checker.classes_with_drop().cloned().chain(cyclic.iter().cloned()).collect();
        owners.sort_by_key(|o| self.checker.class_type_id(o));
        owners.dedup();
        owners
            .into_iter()
            .filter_map(|owner| {
                let label = self.fn_entry_labels.get(&format!("{owner}::drop")).copied();
                let mut type_id = self.checker.class_type_id(&owner);
                if type_id == 0 || type_id >= common::EARLY_DROP_FLAG {
                    return label.map(|l| (native, type_id, Some(l)));
                }
                // A `drop` that keeps nothing of `self` may run where the
                // object is freed early.
                if label.is_some() && self.program_effects.drop_keeps_nothing(&owner) {
                    type_id |= common::EARLY_DROP_FLAG;
                }
                if cyclic.contains(&owner) {
                    type_id |= common::CYCLIC_TYPE_FLAG;
                }
                (label.is_some() || type_id & common::CYCLIC_TYPE_FLAG != 0).then_some((native, type_id, label))
            })
            .collect()
    }

    /// The classes whose objects can close a reference cycle (reference-count
    /// plan step 9): an identity class (`shared`, or one with a `drop`; a
    /// write to any other copies a shared value) whose field types can
    /// reach itself, and any class whose field types reach a function or
    /// coroutine (a `mut` capture names its holder without a copy) or a type
    /// parameter (it may be either). Every cycle holds one of their objects,
    /// so the cycle collector starts from those alone.
    fn cyclic_classes(&self) -> std::collections::BTreeSet<String> {
        let mut out = std::collections::BTreeSet::new();
        for class in self.checker.class_names() {
            let identity = self.checker.is_shared_class(&class) || self.checker.class_has_drop(&class);
            let mut seen = std::collections::HashSet::new();
            let (back, wild) = self.type_reach_class(&class, &class, &mut seen);
            if wild || (identity && back) {
                out.insert(class);
            }
        }
        out
    }

    /// Whether the fields of class `name` reach class `target`, and whether
    /// they reach a function, a coroutine or a type parameter.
    fn type_reach_class(&self, name: &str, target: &str, seen: &mut std::collections::HashSet<String>) -> (bool, bool) {
        if !seen.insert(name.to_string()) {
            return (false, false);
        }
        let mut acc = (false, false);
        let mut tys: Vec<Ty> = Vec::new();
        if let Some(fields) = self.checker.class_fields(name) {
            tys.extend(fields.into_iter().map(|(_, t)| t));
        } else if let Some(variants) = self.checker.enum_variants(name) {
            tys.extend(variants.into_iter().flat_map(|(_, _, p)| p));
        }
        for t in &tys {
            let (b, w) = self.ty_reach_class(t, target, seen);
            acc = (acc.0 || b, acc.1 || w);
        }
        acc
    }

    /// [`Self::type_reach_class`] of a type.
    fn ty_reach_class(&self, ty: &Ty, target: &str, seen: &mut std::collections::HashSet<String>) -> (bool, bool) {
        let all = |me: &Self, tys: &mut dyn Iterator<Item = &Ty>, seen: &mut std::collections::HashSet<String>| {
            tys.fold((false, false), |acc, t| {
                let (b, w) = me.ty_reach_class(t, target, seen);
                (acc.0 || b, acc.1 || w)
            })
        };
        match ty {
            Ty::Var(_) | Ty::Fun(..) | Ty::Existential { .. } | Ty::Forall { .. } => (false, true),
            Ty::Con(n) => {
                let lower = n.to_ascii_lowercase();
                if matches!(lower.as_str(), "fn" | "polyfn" | "coroutine" | "stream") {
                    return (false, true);
                }
                if n == target || self.checker.resolve_class_key(n).is_some_and(|k| k == target) {
                    return (true, false);
                }
                let key = self.checker.resolve_class_key(n).unwrap_or_else(|| n.clone());
                self.type_reach_class(&key, target, seen)
            }
            Ty::App(head, args) => {
                // A weak handle owns nothing.
                if matches!(head.as_ref(), Ty::Con(n) if n == common::BUILTIN_WEAK_TYPE) {
                    return (false, false);
                }
                let (b, w) = self.ty_reach_class(head, target, seen);
                let (b2, w2) = all(self, &mut args.iter(), seen);
                (b || b2, w || w2)
            }
            Ty::Sum { variants, .. } => all(
                self,
                &mut variants.iter().flat_map(|(_, p)| match p {
                    crate::typechecking::ty::EnumVariantPayloadTy::Unit => Vec::new(),
                    crate::typechecking::ty::EnumVariantPayloadTy::Tuple(ts) => ts.iter().collect(),
                    crate::typechecking::ty::EnumVariantPayloadTy::Record(fs) => fs.iter().map(|(_, t)| t).collect(),
                }),
                seen,
            ),
            Ty::Tuple(ts) => all(self, &mut ts.iter(), seen),
            Ty::Array { element, .. } | Ty::Readonly(element) => self.ty_reach_class(element, target, seen),
            Ty::Record { fields } => all(self, &mut fields.iter().map(|(_, t)| t), seen),
            Ty::Constructor { .. } | Ty::Never => (false, false),
        }
    }

    /// `__module_setup`'s MIR: each step as its ops run, an `extern`'s
    /// failed load or declaration panicking with the error's message.
    fn module_setup_mir(&self, steps: &[SetupStep]) -> Result<MirFunc, String> {
        let mir = |e: crate::mir::MirError| e.to_string();
        let mut b = MirBuilder::new(MODULE_SETUP_FN);
        b.allow_host_edges = true;
        for step in steps {
            match step {
                SetupStep::Static { init, slot, tail, .. } => {
                    let target = self.fn_entry_labels.get(init).copied().ok_or_else(|| format!("{init}: no entry"))?;
                    let func = self.bodies.funcs().iter().find(|f| f.name == *init).and_then(|f| f.mir.as_ref());
                    let ret = func
                        .and_then(|m| m.ret_ty)
                        .filter(|t| t.is_word_lane())
                        .ok_or_else(|| format!("{init}: no one-word MIR return"))?;
                    // A two-word static's initializer returns both words.
                    let ret_hi = match tail {
                        Some(_) => Some(func.and_then(|m| m.ret_hi_ty).ok_or_else(|| format!("{init}: no two-word MIR return"))?),
                        None => None,
                    };
                    let abi = crate::layout::CallAbi { params: Vec::new(), ret, ret_hi };
                    let (value, hi) = b.ins_call(target, Vec::new(), &abi).map_err(mir)?;
                    b.ins_store_static(*slot, value).map_err(mir)?;
                    if let (Some(tail), Some(hi)) = (tail, hi) {
                        b.ins_store_static(*tail, hi).map_err(mir)?;
                    }
                }
                SetupStep::Load { name, slot } => {
                    let name = b.ins_string(*name).map_err(mir)?;
                    let handle = Self::setup_ffi(&mut b, MirFfiOp::Load, 0, vec![name])?;
                    b.ins_store_static(*slot, handle).map_err(mir)?;
                }
                SetupStep::Declare { lib_slot, name, args, ret, operand, slot, addr_slot } => {
                    let lib = b.ins_load_static(*lib_slot, MirTy::I64).map_err(mir)?;
                    let name = b.ins_string(*name).map_err(mir)?;
                    let tags = args
                        .iter()
                        .map(|&t| b.ins_const(MirConst::I64(i64::from(t))))
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(mir)?;
                    let tuple = b.ins_alloc(MirAllocKind::TUPLE, tags).map_err(mir)?;
                    let tuple = b.ins_gc_barrier(MirGcKind::Safepoint, tuple).map_err(mir)?;
                    let ret = b.ins_const(MirConst::I64(i64::from(*ret))).map_err(mir)?;
                    let id = Self::setup_ffi(&mut b, MirFfiOp::Declare, *operand, vec![lib, name, tuple, ret])?;
                    b.ins_store_static(*slot, id).map_err(mir)?;
                    let lib = b.ins_load_static(*lib_slot, MirTy::I64).map_err(mir)?;
                    let addr = Self::setup_ffi(&mut b, MirFfiOp::Addr, 0, vec![lib, id])?;
                    b.ins_store_static(*addr_slot, addr).map_err(mir)?;
                }
            }
        }
        let zero = b.ins_const(MirConst::I64(0)).map_err(mir)?;
        b.ret(Some(zero)).map_err(mir)?;
        b.finish().map_err(mir)
    }

    /// An FFI op whose `Result` is unwrapped: the `Ok` word, or a panic
    /// with the `ffi::Error`'s message.
    fn setup_ffi(b: &mut MirBuilder, op: MirFfiOp, operand: u32, args: Vec<crate::mir::ValueId>) -> Result<crate::mir::ValueId, String> {
        let mir = |e: crate::mir::MirError| e.to_string();
        let obj = b.ins_ffi(op, operand, args).map_err(mir)?;
        let (ok, err) = (b.create_block(), b.create_block());
        let word = b.ins_match_payload(obj, 0, MirTy::I64).map_err(mir)?;
        b.jump_if_match(obj, 0, vec![word], ok, err).map_err(mir)?;
        b.switch_to_block(err);
        let e = b.ins_match_payload(obj, 0, MirTy::HeapRef).map_err(mir)?;
        let msg = b.ins_product_load(e, 1, MirTy::HeapRef).map_err(mir)?;
        b.panic(msg).map_err(mir)?;
        b.switch_to_block(ok);
        Ok(word)
    }
}
