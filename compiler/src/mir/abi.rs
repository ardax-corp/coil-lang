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
//! | Niche Option/Result | 1 | Q8 word lane (match reconstructs as `Br`) |
//! | Two-slot return | — | refuse (P3 LIR) |
//!
//! HostInvoke: LICM hoists scalar-pure math; S3 emits I6-typed hosts except
//! I4 string bytes. User `CALL` uses this map when the callee is already
//! dense, or an open one-word ABI (S3). Q7 one-word self-`CALL` / `TailCall`
//! use that open ABI. `CallIndirect` / two-slot `RETURN` still refuse.
//! HeapRef and niche words are one-word lanes (Q8).

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
/// Q7 self-`CALL` uses the open one-word ABI until this map records the body.
pub type DenseCallMap = HashMap<u32, DenseAbi>;

impl DenseAbi {
    /// Word-layout numeric params + one specialized return. Two-slot refuses.
    pub fn from_func(func: &MirFunc) -> Option<Self> {
        if !matches!(func.ret_layout, MirLayout::Word | MirLayout::HeapNiche) {
            return None;
        }
        let ret = func.ret_ty?;
        if !ret.is_word_lane() {
            return None;
        }
        let params: Vec<MirTy> = func.params.iter().map(|p| func.ty(*p)).collect();
        if params.iter().any(|t| !t.is_word_lane()) {
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
            IlOp::Load { slot, .. } | IlOp::LoadReturnSlot { slot, .. } => {
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
        if !ty.is_word_lane() {
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

    #[test]
    fn live_in_params_counts_fused_return_slot() {
        use crate::il::{IlOp, Label};
        use common::DebugLoc;
        let loc = DebugLoc::unknown();
        let ops = vec![
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 1, loc },
            IlOp::Load { slot: 0, loc },
            IlOp::Bin {
                op: common::Instruction::GEQ,
                loc,
            },
            IlOp::LoadReturnSlot { slot: 2, loc },
        ];
        let mut slot_ty = HashMap::new();
        slot_ty.insert(0, MirTy::I64);
        slot_ty.insert(1, MirTy::I64);
        slot_ty.insert(2, MirTy::I64);
        assert_eq!(
            live_in_params(&ops, &slot_ty),
            Some(vec![MirTy::I64, MirTy::I64, MirTy::I64])
        );
    }

    #[test]
    fn from_func_accepts_heapref_word() {
        let mut b = MirBuilder::new("href");
        let p = b.add_param(MirTy::HeapRef).unwrap();
        b.set_ret_ty(MirTy::HeapRef);
        b.ret(Some(p)).unwrap();
        let f = b.finish().unwrap();
        let abi = DenseAbi::from_func(&f).expect("S3 HeapRef is a word lane");
        assert_eq!(abi.params, vec![MirTy::HeapRef]);
        assert_eq!(abi.ret, MirTy::HeapRef);
    }

    #[test]
    fn from_func_accepts_niche_word() {
        let mut b = MirBuilder::new("opt");
        let p = b.add_param(MirTy::NicheOpt).unwrap();
        b.set_ret_ty(MirTy::NicheOpt);
        b.ret(Some(p)).unwrap();
        let f = b.finish().unwrap();
        let abi = DenseAbi::from_func(&f).expect("Q8 niche is a word lane");
        assert_eq!(abi.params, vec![MirTy::NicheOpt]);
        assert_eq!(abi.ret, MirTy::NicheOpt);
    }

    #[test]
    fn from_func_refuses_two_slot() {
        let mut b = MirBuilder::new("pair");
        let lo = b.add_param(MirTy::I64).unwrap();
        let hi = b.ins_const(MirConst::I64(1)).unwrap();
        b.ret_pair(lo, hi).unwrap();
        let f = b.finish().unwrap();
        assert!(DenseAbi::from_func(&f).is_none());
    }
}
