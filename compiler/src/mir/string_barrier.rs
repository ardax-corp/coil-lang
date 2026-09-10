//! I4 — `FORMAT` and general string ops are a hard MIR barrier.
//!
//! No string subset, no half-lifted Format, no unicode/regex in SSA. Fuse-IL
//! keeps `STRING` / `FORMAT` / `STRINGIFY` / `PRINT`. `string::{from_bytes,
//! to_bytes}` HostInvoke stays off dense (I4 / Q9; I6 types them as
//! impure IO edges).

use common::Instruction;

use crate::il::IlOp;

/// Residual format / stringify opcodes (cold `IlOp::Byte`).
pub fn is_format_inst(inst: Instruction) -> bool {
    matches!(inst, Instruction::FORMAT | Instruction::STRINGIFY)
}

/// IL that must not enter dense specialize or MIR→LIR (I4).
pub fn refuses_string_or_format(op: &IlOp) -> bool {
    refuse_reason(op).is_some()
}

/// Inventory label: `string/io` for table push / print, `format` for FORMAT.
pub fn refuse_reason(op: &IlOp) -> Option<&'static str> {
    match op {
        IlOp::String { .. } | IlOp::Print { .. } => Some("string/io"),
        IlOp::Byte { byte, .. } if is_format_inst(*byte.bytecode()) => Some("format"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{Byte, DebugLoc};

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

    #[test]
    fn string_print_and_format_are_barriers() {
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
        assert!(!refuses_string_or_format(&IlOp::Const { imm: 1, loc }));
    }
}
