//! Typed dense call/return ABI (COI-291 M1).
//!
//! Dense bodies keep `i32` / `i64` / `f32` / `f64` / `bool` in frame slots.
//! A dense→dense `CALL` uses the same one-word Value layout the VM already
//! uses at CALL/RETURN — bits are typed slots, not boxed heap objects.
//!
//! ## Layout
//!
//! | Edge | Words | Slots / stack |
//! |------|-------|----------------|
//! | Args | `arity` | callee slots `0..arity` (same bits as caller `LOAD`s) |
//! | Return | 1 | TOS after `RETURN`; caller `STORE`s into a typed dest |
//! | Two-slot / niche | — | refuse (M4 / P3 LIR) |
//!
//! HostInvoke stays on the W4 allowlist (box → native → unbox). User `CALL`
//! is allowed only when the callee already specialized to this ABI.
//! Self- and mutual-recursion stay refuse until the callee is in the map
//! (leaf-first). `TailCall` / `CallIndirect` refuse.

use std::collections::{HashMap, HashSet};

use crate::il::IlOp;
use common::Instruction;

use super::func::MirFunc;
use super::layout::MirLayout;
use super::ty::MirTy;

/// One-word dense callee signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DenseAbi {
    pub params: Vec<MirTy>,
    pub ret: MirTy,
}

/// Entry-label id → dense ABI. Built leaf-first during specialize.
pub type DenseCallMap = HashMap<u32, DenseAbi>;

impl DenseAbi {
    /// Word-layout numeric params + one specialized return. Two-slot refuses.
    pub fn from_func(func: &MirFunc) -> Option<Self> {
        if func.ret_layout != MirLayout::Word {
            return None;
        }
        let ret = func.ret_ty?;
        if !ret.is_specialized() {
            return None;
        }
        let params: Vec<MirTy> = func.params.iter().map(|p| func.ty(*p)).collect();
        if params.iter().any(|t| !t.is_specialized()) {
            return None;
        }
        Some(Self { params, ret })
    }

    /// Recover params when `IlFunc.entry_sp` is 0 (frame-base seed, not arity).
    pub fn from_func_and_live_ins(
        func: &MirFunc,
        ops: &[IlOp],
        slot_ty: &HashMap<u32, MirTy>,
    ) -> Option<Self> {
        let mut abi = Self::from_func(func)?;
        if abi.params.is_empty() {
            if let Some(params) = live_in_params(ops, slot_ty) {
                abi.params = params;
            }
        }
        Some(abi)
    }
}

/// Prefix `0..arity` of slots read before they are stored (CALL args).
pub fn live_in_params(ops: &[IlOp], slot_ty: &HashMap<u32, MirTy>) -> Option<Vec<MirTy>> {
    let mut stored = HashSet::new();
    let mut live = HashSet::new();
    for op in ops {
        match op {
            IlOp::StorePop { slot, .. } => {
                stored.insert(*slot);
            }
            IlOp::Load { slot, .. } => {
                if !stored.contains(slot) {
                    live.insert(*slot);
                }
            }
            IlOp::BinSlotImm { slot, .. } => {
                if !stored.contains(&u32::from(*slot)) {
                    live.insert(u32::from(*slot));
                }
            }
            IlOp::BinSlotSlot { a, b, .. } => {
                let a = u32::from(*a);
                let b = u32::from(*b);
                if !stored.contains(&a) {
                    live.insert(a);
                }
                if !stored.contains(&b) {
                    live.insert(b);
                }
            }
            IlOp::Byte { byte, .. }
                if matches!(
                    *byte.bytecode(),
                    Instruction::INC | Instruction::DEC
                ) =>
            {
                let (slot, _, _) = byte.inc_dec_parts();
                let slot = slot as u32;
                if !stored.contains(&slot) {
                    live.insert(slot);
                }
            }
            _ => {}
        }
    }
    if live.is_empty() {
        return Some(Vec::new());
    }
    let max = live.iter().copied().max()?;
    if live.len() != max as usize + 1 || (0..=max).any(|i| !live.contains(&i)) {
        return None;
    }
    let mut params = Vec::with_capacity(max as usize + 1);
    for i in 0..=max {
        let ty = *slot_ty.get(&i)?;
        if !ty.is_specialized() {
            return None;
        }
        params.push(ty);
    }
    Some(params)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::builder::MirBuilder;
    use crate::mir::inst::MirConst;

    #[test]
    fn from_func_records_word_abi() {
        let mut b = MirBuilder::new("leaf");
        let x = b.add_param(MirTy::F64).unwrap();
        let y = b.add_param(MirTy::I64).unwrap();
        let _ = y;
        b.set_ret_ty(MirTy::F64);
        b.ret(Some(x)).unwrap();
        let f = b.finish().unwrap();
        let abi = DenseAbi::from_func(&f).expect("word abi");
        assert_eq!(abi.params, vec![MirTy::F64, MirTy::I64]);
        assert_eq!(abi.ret, MirTy::F64);
    }

    #[test]
    fn live_in_params_reads_prefix() {
        use crate::il::{IlOp, Label};
        use common::DebugLoc;
        let loc = DebugLoc::unknown();
        let ops = vec![
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 0, loc },
            IlOp::Load { slot: 1, loc },
            IlOp::Bin {
                op: common::Instruction::ADDF,
                loc,
            },
            IlOp::StorePop { slot: 2, loc },
            IlOp::Load { slot: 2, loc },
            IlOp::Return {
                loc,
                ret_words: 1,
            },
        ];
        let mut slot_ty = HashMap::new();
        slot_ty.insert(0, MirTy::F64);
        slot_ty.insert(1, MirTy::F64);
        slot_ty.insert(2, MirTy::F64);
        assert_eq!(
            live_in_params(&ops, &slot_ty),
            Some(vec![MirTy::F64, MirTy::F64])
        );
    }

    fn from_func_refuses_two_slot() {
        let mut b = MirBuilder::new("pair");
        let lo = b.add_param(MirTy::I64).unwrap();
        let hi = b.ins_const(MirConst::I64(1)).unwrap();
        b.ret_pair(lo, hi).unwrap();
        let f = b.finish().unwrap();
        assert!(DenseAbi::from_func(&f).is_none());
    }
}
