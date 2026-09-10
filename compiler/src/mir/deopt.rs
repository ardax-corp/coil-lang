//! I7 — debugger stop / deopt boundaries on MIR edges.
//!
//! The VM debugger on fuse-IL bytecode remains the v1 stop engine. This
//! sidecar names where a later native or denser tier must pause or leave
//! specialized code. Production specialize does not set
//! [`crate::mir::LowerHints::allow_deopt`]; debugger-attached compiles
//! refuse dense / MIR→LIR replace instead.

use crate::il::IlOp;

use super::effects::host_is_pure;
use super::gc::is_alloc_inst;
use super::inst::{MirDeoptKind, MirInst};

impl MirInst {
    /// Kind if this inst is a leave-or-stop boundary (explicit or implicit).
    pub fn deopt_kind(&self) -> Option<MirDeoptKind> {
        match self {
            Self::Deopt { kind, .. } => Some(*kind),
            Self::Call { .. }
            | Self::Alloc { .. }
            | Self::GcBarrier { .. }
            | Self::Print { .. }
            | Self::Format { .. }
            | Self::Stringify { .. } => Some(MirDeoptKind::Deopt),
            Self::HostInvoke { native_id, .. } if !host_is_pure(*native_id) => {
                Some(MirDeoptKind::Deopt)
            }
            _ => None,
        }
    }
}

/// IL ops that become an explicit [`MirInst::Deopt`] when `allow_deopt`.
pub fn boundary_for_op(op: &IlOp) -> Option<MirDeoptKind> {
    match op {
        IlOp::HostInvoke { .. }
        | IlOp::Entry { .. }
        | IlOp::MakeArray { .. }
        | IlOp::MakeTuple { .. }
        | IlOp::MakeEnum { .. }
        | IlOp::Print { .. } => Some(MirDeoptKind::Deopt),
        IlOp::Byte { byte, .. }
            if is_alloc_inst(*byte.bytecode())
                || matches!(
                    *byte.bytecode(),
                    common::Instruction::FORMAT | common::Instruction::STRINGIFY
                ) =>
        {
            Some(MirDeoptKind::Deopt)
        }
        IlOp::Return { .. } | IlOp::Halt { .. } | IlOp::Jump { .. } => Some(MirDeoptKind::Stop),
        IlOp::StorePop { loc, .. } if loc.is_known() => Some(MirDeoptKind::Stop),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::il::Label;
    use common::DebugLoc;
    use super::super::inst::ValueId;

    fn loc() -> DebugLoc {
        DebugLoc {
            file: 0,
            start_byte: 0,
            end_byte: 4,
        }
    }

    #[test]
    fn implicit_call_and_explicit_stop() {
        let call = MirInst::Call {
            dest: ValueId(0),
            target: Label(1),
            args: Vec::new(),
        };
        assert_eq!(call.deopt_kind(), Some(MirDeoptKind::Deopt));
        let stop = MirInst::Deopt {
            dest: ValueId(1),
            kind: MirDeoptKind::Stop,
            loc: loc(),
        };
        assert!(stop.is_deopt_edge());
        assert_eq!(stop.deopt_kind(), Some(MirDeoptKind::Stop));
        assert!(
            boundary_for_op(&IlOp::Return {
                loc: loc(),
                ret_words: 1
            })
            .is_some()
        );
        assert!(
            boundary_for_op(&IlOp::Const {
                imm: 1,
                loc: loc()
            })
            .is_none()
        );
    }
}
