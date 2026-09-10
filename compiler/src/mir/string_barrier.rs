//! I4 / Q9 — string / format ladder (not a permanent MIR barrier).
//!
//! R1: table `STRING` / `PRINT` / `FORMAT` / `STRINGIFY` may enter
//! MIR→LIR. Dense infer still refuses so numeric specialize is unchanged.
//! `string::{from_bytes,to_bytes}` are I6 dense HostInvoke (R2). Unicode /
//! regex stay out (R4 / B9).

use common::Instruction;

use crate::il::IlOp;

/// Residual format / stringify opcodes (cold `IlOp::Byte`).
pub fn is_format_inst(inst: Instruction) -> bool {
    matches!(inst, Instruction::FORMAT | Instruction::STRINGIFY)
}

/// Table / print / format IL (R1 reconstruct set).
pub fn is_string_il(op: &IlOp) -> bool {
    match op {
        IlOp::String { .. } | IlOp::Print { .. } => true,
        IlOp::Byte { byte, .. } => matches!(
            *byte.bytecode(),
            Instruction::STRING | Instruction::PRINT | Instruction::FORMAT | Instruction::STRINGIFY
        ),
        _ => false,
    }
}

/// Inventory label: `string/io` for table push / print, `format` for FORMAT.
pub fn refuse_reason(op: &IlOp) -> Option<&'static str> {
    match op {
        IlOp::String { .. } | IlOp::Print { .. } => Some("string/io"),
        IlOp::Byte { byte, .. } if is_format_inst(*byte.bytecode()) => Some("format"),
        IlOp::Byte { byte, .. }
            if matches!(
                *byte.bytecode(),
                Instruction::STRING | Instruction::PRINT
            ) =>
        {
            Some("string/io")
        }
        _ => None,
    }
}

/// Dense specialize still refuses table `STRING` / `PRINT` / `FORMAT` /
/// `STRINGIFY` (R1). Byte hosts are HostInvoke, not this set.
pub fn refuses_dense_string(op: &IlOp) -> bool {
    is_string_il(op)
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{Byte, DebugLoc};

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

    #[test]
    fn string_print_and_format_are_r1_il() {
        let loc = loc();
        assert_eq!(
            refuse_reason(&IlOp::String { idx: 0, loc }),
            Some("string/io")
        );
        assert_eq!(refuse_reason(&IlOp::Print { loc }), Some("string/io"));
        assert_eq!(
            refuse_reason(&IlOp::Byte {
                byte: Byte::new(Instruction::FORMAT).with_operand_u32(1),
                loc,
            }),
            Some("format")
        );
        assert_eq!(
            refuse_reason(&IlOp::Byte {
                byte: Byte::new(Instruction::STRINGIFY),
                loc,
            }),
            Some("format")
        );
        assert!(is_string_il(&IlOp::String { idx: 0, loc }));
        assert!(refuses_dense_string(&IlOp::Print { loc }));
        assert!(!is_string_il(&IlOp::Const { imm: 1, loc }));
    }
}
