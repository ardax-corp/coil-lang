//! Pure-call context for IL passes that refuse impure `CALL` barriers (COI-99).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use common::Instruction;

use super::op::{EntryKind, IlJumpKind, IlOp, Label};

/// Maps entry labels to callee names plus the purity closure from the AST.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PureCallCtx {
    pub pure_fns: HashSet<String>,
    pub label_callees: HashMap<u32, String>,
}

impl PureCallCtx {
    pub fn call_is_pure(&self, target: Label) -> bool {
        self.label_callees
            .get(&target.0)
            .is_some_and(|n| self.pure_fns.contains(n))
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

/// True when `op` blocks length-invariance / ArrayLen hoist for an array loop.
pub fn op_blocks_length_proof(op: &IlOp) -> bool {
    let ctx = active_ctx();
    match op {
        IlOp::HostInvoke { .. } | IlOp::Print { .. } => true,
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
        IlOp::Byte { byte, .. } => matches!(
            *byte.bytecode(),
            Instruction::HostInvoke
                | Instruction::PRINT
                | Instruction::CALL
                | Instruction::FORMAT
                | Instruction::FfiInvoke
        ),
        _ => false,
    }
}

/// True when `op` blocks LICM / field-sensitive hoists (GetField allowed).
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
        IlOp::Byte { byte, .. } => matches!(
            *byte.bytecode(),
            Instruction::HostInvoke
                | Instruction::PRINT
                | Instruction::CALL
                | Instruction::FfiInvoke
                | Instruction::SetField
                | Instruction::GetField
        ),
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
}
