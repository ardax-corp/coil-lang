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

use std::collections::HashMap;

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
    fn from_func_refuses_two_slot() {
        let mut b = MirBuilder::new("pair");
        let lo = b.add_param(MirTy::I64).unwrap();
        let hi = b.ins_const(MirConst::I64(1)).unwrap();
        b.ret_pair(lo, hi).unwrap();
        let f = b.finish().unwrap();
        assert!(DenseAbi::from_func(&f).is_none());
    }
}
