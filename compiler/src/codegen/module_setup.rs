//! Module setup as a function of its own (`__module_setup`): the static
//! initializers' stores and each `extern` library's load and declarations.
//! The finalizer registry is one more (`__finalizer_setup`), called first of
//! all. The prologue calls them before `main`
//! (`CALL __module_setup; POP; JMP main`), so it is a body like any other:
//! the register tiers emit it from its MIR and native code compiles it
//! (`docs/internals/native.md`, "Module setup").
//!
//! The initializers computed while compiling (`const`s and pure `static
//! let`s, [`ComptimeConst`]) stay in the prologue ahead of that call:
//! `Pipeline::compute_consts` replaces their calls with the values'
//! snapshot (`RestoreSnapshot`), and one that cannot be computed keeps its
//! startup call, which runs natively like any call.

use super::*;
use crate::mir::{MirAllocKind, MirBuilder, MirConst, MirFfiOp, MirFunc, MirGcKind, MirTy};

/// The setup function's name.
pub(crate) const MODULE_SETUP_FN: &str = "__module_setup";

/// The finalizer registry's function name.
const FINALIZER_SETUP_FN: &str = "__finalizer_setup";

/// One step of module setup, in the order its ops run.
pub(crate) enum SetupStep {
    /// `static_slot = init()`; `comptime` when the value is computed while
    /// compiling (it stays in the prologue).
    Static { init: String, slot: u32, comptime: bool },
    /// Load the library named by string `name`; its handle to `slot`.
    Load { name: u32, slot: u32 },
    /// Declare symbol `name` of the library in `lib_slot` with argument
    /// type words `args` and return type word `ret` (`DeclareFFI`'s
    /// `operand`); its function id to `slot`.
    Declare { lib_slot: u32, name: u32, args: Vec<u32>, ret: u32, operand: u32, slot: u32 },
}

impl Compiler {
    /// Emit `__module_setup` from the setup collected while compiling, and
    /// the prologue's call to it at `insert_at`; the call's length, `None`
    /// when the program has nothing to set up.
    pub(super) fn emit_module_setup(&mut self, insert_at: usize) -> Option<usize> {
        // The setup ops are rebuilt from the steps: `static_init` only
        // served the capability scan.
        drop(std::mem::take(&mut self.static_init));
        let finalizers = self.finalizer_registry();
        let steps = std::mem::take(&mut self.setup_steps);
        let mut region = CodeBuf::new();
        let mut setup = Vec::new();
        for step in steps {
            match step {
                SetupStep::Static { ref init, slot, comptime } => {
                    let Some(&offset) = self.functions.get(init) else { continue };
                    // A `comptime` value's call stays in the prologue, where
                    // `compute_consts` replaces it.
                    if comptime {
                        region.push(Self::packed_entry_byte_ret(crate::il::EntryKind::Call, 0, offset as u32, 1));
                        region.push(Byte::new(Instruction::StoreStatic).with_operand_u32(slot));
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
            let (offset, label) = self.bind_function_entry(name.clone());
            let start = self.bytecode.len();
            // Its code is emitted from the MIR below.
            self.bytecode.push_return();
            let end = self.bytecode.len();
            self.record_fn_span(name.clone(), start, end);
            self.bytecode.record_func_with_sp(name, Some(label), start, end, 0);
            match self.module_setup_mir(&setup) {
                Ok(mut func) => {
                    func.shrink_to_fit();
                    self.bytecode.set_last_func_hir_mir(func);
                }
                Err(why) => self.bytecode.set_last_func_hir_mir_why(why),
            }
            region.push(Self::packed_entry_byte_ret(crate::il::EntryKind::Call, 0, offset as u32, 1));
            region.push_pop();
        }
        // The registry runs before every initializer, the prologue's included:
        // a collection while one runs must find the `drop` of what it made.
        if !finalizers.is_empty() {
            let offset = self.emit_finalizer_setup(&finalizers);
            let mut first = CodeBuf::new();
            first.push(Self::packed_entry_byte_ret(crate::il::EntryKind::Call, 0, offset as u32, 1));
            first.push_pop();
            first.append(&mut region);
            region = first;
        }
        if region.is_empty() {
            return None;
        }
        let n = region.len();
        self.bytecode.splice_buf_at(insert_at, region);
        Some(n)
    }

    /// Emit `__finalizer_setup`, the registry of `finalizers`; its entry.
    fn emit_finalizer_setup(&mut self, finalizers: &[(usize, u32, crate::il::Label)]) -> usize {
        let name = FINALIZER_SETUP_FN.to_string();
        let (offset, label) = self.bind_function_entry(name.clone());
        let start = self.bytecode.len();
        // Its code is emitted from the MIR below.
        self.bytecode.push_return();
        let end = self.bytecode.len();
        self.record_fn_span(name.clone(), start, end);
        self.bytecode.record_func_with_sp(name, Some(label), start, end, 0);
        match Self::finalizer_setup_mir(finalizers) {
            Ok(mut func) => {
                func.shrink_to_fit();
                self.bytecode.set_last_func_hir_mir(func);
            }
            Err(why) => self.bytecode.set_last_func_hir_mir_why(why),
        }
        offset
    }

    /// `__finalizer_setup`'s MIR.
    fn finalizer_setup_mir(finalizers: &[(usize, u32, crate::il::Label)]) -> Result<MirFunc, String> {
        let mir = |e: crate::mir::MirError| e.to_string();
        let mut b = MirBuilder::new(FINALIZER_SETUP_FN);
        b.allow_host_edges = true;
        for &(native, type_id, drop) in finalizers {
            let type_id = b.ins_const(MirConst::I64(i64::from(type_id))).map_err(mir)?;
            let drop = b.ins_code_ptr(drop).map_err(mir)?;
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
    /// entry)` for each class with a `drop`, in type-id order (archives
    /// stay reproducible).
    fn finalizer_registry(&self) -> Vec<(usize, u32, crate::il::Label)> {
        let Some(native) = self.native_id("gc_register_finalizer") else {
            return Vec::new();
        };
        let mut owners: Vec<String> = self.checker.classes_with_drop().cloned().collect();
        owners.sort_by_key(|o| self.checker.class_type_id(o));
        owners
            .into_iter()
            .filter_map(|owner| {
                let label = self.fn_entry_labels.get(&format!("{owner}::drop")).copied()?;
                let mut type_id = self.checker.class_type_id(&owner);
                // A `drop` that keeps nothing of `self` may run where the
                // object is freed early.
                if type_id < common::EARLY_DROP_FLAG && self.program_effects.drop_keeps_nothing(&owner) {
                    type_id |= common::EARLY_DROP_FLAG;
                }
                Some((native, type_id, label))
            })
            .collect()
    }

    /// `__module_setup`'s MIR: each step as its ops run, an `extern`'s
    /// failed load or declaration panicking with the error's message.
    fn module_setup_mir(&self, steps: &[SetupStep]) -> Result<MirFunc, String> {
        let mir = |e: crate::mir::MirError| e.to_string();
        let mut b = MirBuilder::new(MODULE_SETUP_FN);
        b.allow_host_edges = true;
        for step in steps {
            match step {
                SetupStep::Static { init, slot, .. } => {
                    let target = self.fn_entry_labels.get(init).copied().ok_or_else(|| format!("{init}: no entry"))?;
                    let ret = self
                        .bytecode
                        .funcs()
                        .iter()
                        .find(|f| f.name == *init)
                        .and_then(|f| f.hir_mir.as_ref())
                        .and_then(|m| m.ret_ty)
                        .filter(|t| t.is_word_lane())
                        .ok_or_else(|| format!("{init}: no one-word MIR return"))?;
                    let abi = crate::layout::DenseAbi { params: Vec::new(), ret, ret_hi: None };
                    let (value, _) = b.ins_call(target, Vec::new(), &abi).map_err(mir)?;
                    b.ins_store_static(*slot, value).map_err(mir)?;
                }
                SetupStep::Load { name, slot } => {
                    let name = b.ins_string(*name).map_err(mir)?;
                    let handle = Self::setup_ffi(&mut b, MirFfiOp::Load, 0, vec![name])?;
                    b.ins_store_static(*slot, handle).map_err(mir)?;
                }
                SetupStep::Declare { lib_slot, name, args, ret, operand, slot } => {
                    let lib = b.ins_load_static(*lib_slot, MirTy::I64).map_err(mir)?;
                    let name = b.ins_string(*name).map_err(mir)?;
                    let tags = args
                        .iter()
                        .map(|&t| b.ins_const(MirConst::I64(i64::from(t))))
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(mir)?;
                    let tuple = b.ins_alloc(MirAllocKind::Tuple, tags).map_err(mir)?;
                    let tuple = b.ins_gc_barrier(MirGcKind::Safepoint, vec![tuple]).map_err(mir)?;
                    let ret = b.ins_const(MirConst::I64(i64::from(*ret))).map_err(mir)?;
                    let id = Self::setup_ffi(&mut b, MirFfiOp::Declare, *operand, vec![lib, name, tuple, ret])?;
                    b.ins_store_static(*slot, id).map_err(mir)?;
                }
            }
        }
        let zero = b.ins_const(MirConst::I64(0)).map_err(mir)?;
        b.ret(Some(zero)).map_err(mir)?;
        b.finish().map_err(mir)
    }

    /// An FFI op whose `Result` is unwrapped: the `Ok` word, or a panic
    /// with the `ffi::Error`'s message (`emit_result_unwrap_or_panic`).
    fn setup_ffi(b: &mut MirBuilder, op: MirFfiOp, operand: u32, args: Vec<crate::mir::ValueId>) -> Result<crate::mir::ValueId, String> {
        let mir = |e: crate::mir::MirError| e.to_string();
        let obj = b.ins_ffi(op, operand, args).map_err(mir)?;
        let (ok, err) = (b.create_block(), b.create_block());
        let word = b.ins_match_payload(obj, 0, MirTy::I64).map_err(mir)?;
        b.jump_if_match(obj, 0, vec![word], ok, err).map_err(mir)?;
        b.switch_to_block(err);
        let e = b.ins_match_payload(obj, 0, MirTy::HeapRef).map_err(mir)?;
        let msg = b.ins_heap_field_load(e, 1, MirTy::HeapRef).map_err(mir)?;
        b.panic(msg).map_err(mir)?;
        b.switch_to_block(ok);
        Ok(word)
    }
}
