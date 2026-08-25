//! Pure-call context for IL passes that refuse impure `CALL` barriers (COI-99).
//!
//! Reuses [`crate::typechecking::analyze_pure_fns`] (auto-par's whole-function
//! purity). A callee is length-safe only when that set contains its bind name
//! (or a single-segment `mod::f` / `Type::m` suffix). Anything the lattice
//! cannot prove — host / FFI / `FORMAT`, field get/set, `CallIndirect`,
//! `ArrayPush` in the callee — stays a barrier.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use common::{Byte, Instruction};

use super::op::{EntryKind, IlJumpKind, IlOp, Label};

/// Maps entry labels and packed CALL offsets to callee names plus the AST purity set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PureCallCtx {
    pub pure_fns: HashSet<String>,
    pub label_callees: HashMap<u32, String>,
    /// Emit-time `CALL` targets (`self.functions` offsets) → bind names.
    pub offset_callees: HashMap<u32, String>,
}

impl PureCallCtx {
    pub fn call_is_pure(&self, target: Label) -> bool {
        self.label_callees
            .get(&target.0)
            .is_some_and(|n| self.name_is_pure(n))
    }

    pub fn call_offset_is_pure(&self, target: u32) -> bool {
        self.offset_callees
            .get(&target)
            .is_some_and(|n| self.name_is_pure(n))
    }

    /// Exact bind name, or a single `::` suffix against the AST short name.
    fn name_is_pure(&self, name: &str) -> bool {
        if self.pure_fns.contains(name) {
            return true;
        }
        match name.rsplit_once("::") {
            Some((prefix, short)) if !prefix.contains("::") => self.pure_fns.contains(short),
            _ => false,
        }
    }
}

thread_local! {
    static PURE_CALL_CTX: RefCell<Option<PureCallCtx>> = const { RefCell::new(None) };
}

/// Install purity facts for the next [`super::bounds::loop_bounds`] / LICM run.
pub fn set_pure_call_ctx(ctx: Option<PureCallCtx>) {
    PURE_CALL_CTX.with(|c| *c.borrow_mut() = ctx);
}

pub(crate) fn with_pure_call_ctx<R>(f: impl FnOnce(Option<PureCallCtx>) -> R) -> R {
    PURE_CALL_CTX.with(|c| f(c.borrow().clone()))
}

fn active_ctx() -> Option<PureCallCtx> {
    PURE_CALL_CTX.with(|c| c.borrow().clone())
}

fn call_byte_is_pure(byte: &Byte, ctx: Option<&PureCallCtx>) -> bool {
    if *byte.bytecode() != Instruction::CALL {
        return false;
    }
    let (_, target) = byte.call_parts();
    ctx.is_some_and(|c| c.call_offset_is_pure(target as u32))
}

/// True when `op` blocks length-invariance / ArrayLen hoist for an array loop.
pub fn op_blocks_length_proof(op: &IlOp) -> bool {
    let ctx = active_ctx();
    match op {
        IlOp::HostInvoke { .. } | IlOp::Print { .. } => true,
        IlOp::GetField { .. } | IlOp::SetField { .. } => true,
        IlOp::Entry {
            kind: EntryKind::Call,
            target,
            ..
        } => !ctx.as_ref().is_some_and(|c| c.call_is_pure(*target)),
        IlOp::Entry { .. } => true,
        IlOp::Jump {
            kind: IlJumpKind::JumpIfMatch { .. },
            ..
        } => true,
        IlOp::Byte { byte, .. } => match *byte.bytecode() {
            Instruction::HostInvoke
            | Instruction::PRINT
            | Instruction::FORMAT
            | Instruction::FfiInvoke
            | Instruction::CallIndirect
            | Instruction::GetField
            | Instruction::SetField
            | Instruction::TailCall
            // Resume restores empty pin maps; pins are not saved on ObjCoroutine.
            | Instruction::YieldCoro
            | Instruction::YieldFromCoro => true,
            Instruction::CALL => !call_byte_is_pure(byte, ctx.as_ref()),
            _ => false,
        },
        _ => false,
    }
}

/// True when `op` blocks LICM / field-sensitive hoists.
pub fn op_blocks_licm(op: &IlOp) -> bool {
    let ctx = active_ctx();
    match op {
        IlOp::HostInvoke { .. } | IlOp::Print { .. } => true,
        IlOp::SetField { .. } | IlOp::GetField { .. } => true,
        IlOp::Entry {
            kind: EntryKind::Call,
            target,
            ..
        } => !ctx.as_ref().is_some_and(|c| c.call_is_pure(*target)),
        IlOp::Entry { .. } => true,
        IlOp::Jump {
            kind: IlJumpKind::JumpIfMatch { .. },
            ..
        } => true,
        IlOp::Byte { byte, .. } => match *byte.bytecode() {
            Instruction::HostInvoke
            | Instruction::PRINT
            | Instruction::FfiInvoke
            | Instruction::SetField
            | Instruction::GetField => true,
            Instruction::CALL => !call_byte_is_pure(byte, ctx.as_ref()),
            _ => false,
        },
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use common::DebugLoc;

    use super::*;

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

    #[test]
    fn pure_call_entry_is_not_a_length_barrier() {
        let mut ctx = PureCallCtx::default();
        ctx.pure_fns.insert("sq".into());
        ctx.label_callees.insert(7, "sq".into());
        set_pure_call_ctx(Some(ctx));
        let op = IlOp::Entry {
            kind: EntryKind::Call,
            arity: 1,
            target: Label(7),
            loc: loc(),
        };
        assert!(!op_blocks_length_proof(&op));
        set_pure_call_ctx(None);
    }

    #[test]
    fn impure_call_entry_stays_a_barrier() {
        set_pure_call_ctx(None);
        let op = IlOp::Entry {
            kind: EntryKind::Call,
            arity: 1,
            target: Label(1),
            loc: loc(),
        };
        assert!(op_blocks_length_proof(&op));
    }

    #[test]
    fn pure_call_byte_offset_is_not_a_length_barrier() {
        let mut ctx = PureCallCtx::default();
        ctx.pure_fns.insert("sq".into());
        ctx.offset_callees.insert(42, "sq".into());
        set_pure_call_ctx(Some(ctx));
        let op = IlOp::Byte {
            byte: Byte::new(Instruction::CALL).with_call_packed(1, 42),
            loc: loc(),
        };
        assert!(!op_blocks_length_proof(&op));
        set_pure_call_ctx(None);
    }

    #[test]
    fn unknown_call_byte_stays_a_barrier() {
        set_pure_call_ctx(None);
        let op = IlOp::Byte {
            byte: Byte::new(Instruction::CALL).with_call_packed(1, 42),
            loc: loc(),
        };
        assert!(op_blocks_length_proof(&op));
    }

    #[test]
    fn call_indirect_and_field_ops_stay_barriers() {
        set_pure_call_ctx(None);
        assert!(op_blocks_length_proof(&IlOp::Byte {
            byte: Byte::new(Instruction::CallIndirect),
            loc: loc(),
        }));
        assert!(op_blocks_length_proof(&IlOp::GetField { loc: loc() }));
        assert!(op_blocks_length_proof(&IlOp::SetField { loc: loc() }));
    }

    #[test]
    fn yield_ops_are_length_proof_barriers() {
        set_pure_call_ctx(None);
        assert!(op_blocks_length_proof(&IlOp::Byte {
            byte: Byte::new(Instruction::YieldCoro),
            loc: loc(),
        }));
        assert!(op_blocks_length_proof(&IlOp::Byte {
            byte: Byte::new(Instruction::YieldFromCoro),
            loc: loc(),
        }));
        assert!(op_blocks_length_proof(&IlOp::Byte {
            byte: Byte::new(Instruction::TailCall).with_call_packed(1, 0),
            loc: loc(),
        }));
    }

    #[test]
    fn module_qualified_pure_name_matches_ast_short_name() {
        let mut ctx = PureCallCtx::default();
        ctx.pure_fns.insert("sq".into());
        ctx.label_callees.insert(3, "util::sq".into());
        assert!(ctx.call_is_pure(Label(3)));
        ctx.label_callees.insert(4, "mod::Type::sq".into());
        assert!(!ctx.call_is_pure(Label(4)));
    }
}
