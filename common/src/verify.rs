//! Load-time bytecode verification.
//!
//! The VM decodes operands with `promise!` + unchecked reads, so a corrupt or
//! hand-edited archive would be undefined behaviour rather than an error.
//! [`verify_bytecode`] checks every operand the interpreter trusts without a
//! runtime test: retired opcodes, jump / call targets, constant-pool,
//! string-table and static-slot indices. Frame-slot operands and stack height
//! are still compiler invariants (they need per-function frame sizes).

use crate::opcode::{Byte, Instruction};

/// Why an archive's bytecode was rejected, with the offending PC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BytecodeError {
    pub pc: usize,
    pub reason: String,
}

impl std::fmt::Display for BytecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid bytecode at pc {}: {}", self.pc, self.reason)
    }
}

impl std::error::Error for BytecodeError {}

/// Table sizes the bytecode may index into.
#[derive(Debug, Clone, Copy)]
pub struct VerifyLimits {
    pub strings: usize,
    pub static_slots: usize,
}

/// Opcodes kept only so discriminants stay stable; the VM panics on them.
pub fn is_retired(op: Instruction) -> bool {
    use Instruction::*;
    matches!(
        op,
        DATA | SET
            | OptionNicheToHeap
            | HeapOptionToNiche
            | PairJumpIfTag
            | PairToHeap
            | HeapToPair
            | ReturnPair
            | HostInvokeNiche
            | FloatChainStore
            | BinSlotSlotConstJmpf
    )
}

/// Check every word of `code` against `limits`. See the module docs for scope.
pub fn verify_bytecode(
    code: &[Byte],
    constants: &[u64],
    limits: VerifyLimits,
) -> Result<(), BytecodeError> {
    let len = code.len();
    for (pc, byte) in code.iter().enumerate() {
        let fail = |reason: String| Err(BytecodeError { pc, reason });
        let op = *byte.bytecode();
        let operand = byte.operand_u32();
        // `code.len()` is a legal "fall out of the program" jump target.
        let jump = |target: usize, what: &str| {
            if target > len {
                fail(format!("{what} target {target} past end ({len})"))
            } else {
                Ok(())
            }
        };
        let entry = |target: usize, what: &str| {
            if target >= len {
                fail(format!("{what} entry {target} out of range ({len})"))
            } else {
                Ok(())
            }
        };
        let pool = |idx: usize, what: &str| -> Result<u64, BytecodeError> {
            constants.get(idx).copied().ok_or_else(|| BytecodeError {
                pc,
                reason: format!("{what} pool index {idx} out of range ({})", constants.len()),
            })
        };
        if is_retired(op) {
            return fail(format!("retired opcode {}", op as u8));
        }
        use Instruction::*;
        match op {
            JMP | JMPF | JMPT => jump(operand as usize, "jump")?,
            CALL | TailCall | MakeCoro => entry(byte.call_parts().1, "call")?,
            CodePtr | MakePolyFn => entry(operand as usize, "function")?,
            CONST if operand & Byte::POOL_FLAG != 0 => {
                pool((operand & !Byte::POOL_FLAG) as usize, "CONST")?;
            }
            DenseConst if operand & (1 << 31) != 0 => {
                pool((operand & 0xFFFF) as usize, "DenseConst")?;
            }
            STRING if operand as usize >= limits.strings => {
                return fail(format!(
                    "string index {operand} out of range ({})",
                    limits.strings
                ));
            }
            LoadStatic | StoreStatic if operand as usize >= limits.static_slots => {
                return fail(format!(
                    "static slot {operand} out of range ({})",
                    limits.static_slots
                ));
            }
            JumpIfMatch => {
                let target = pool((operand & 0xFFFF) as usize, "JumpIfMatch")?;
                jump(target as usize, "JumpIfMatch")?;
            }
            CmpJmpf | CmpJmpt => {
                let t = byte.cmp_jmpf_parts().1;
                let target = if byte.cmp_jmpf_is_pool() {
                    pool(t, "CmpJmp")? as usize
                } else {
                    t
                };
                jump(target, "CmpJmp")?;
            }
            LogNotJmpf | LogNotJmpt => {
                let t = byte.log_not_jmpf_target();
                let target = if byte.log_not_jmpf_is_pool() {
                    pool(t, "LogNotJmp")? as usize
                } else {
                    t
                };
                jump(target, "LogNotJmp")?;
            }
            BinSlotImmJmpf | BinSlotImmJmpt => {
                let desc = pool(byte.bin_slot_imm_jmpf_parts().2, "BinSlotImmJmp")?;
                jump((desc >> 32) as usize, "BinSlotImmJmp")?;
            }
            BinSlotSlotJmpf | BinSlotSlotJmpt => {
                let desc = pool(byte.bin_slot_slot_jmpf_parts().2, "BinSlotSlotJmp")?;
                jump((desc >> 32) as usize, "BinSlotSlotJmp")?;
            }
            BinSlotSlotConstJmpt => {
                let desc = pool(byte.bin_slot_slot_const_jmpf_parts().2, "BinSlotSlotConstJmp")?;
                let (_, _, float_idx, target) = Byte::unpack_bin_slot_slot_const_jmpf_desc(desc);
                pool(float_idx, "BinSlotSlotConstJmp float")?;
                jump(target, "BinSlotSlotConstJmp")?;
            }
            BinSlotImmStore => {
                pool(byte.bin_slot_imm_store_parts().2, "BinSlotImmStore")?;
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> VerifyLimits {
        VerifyLimits {
            strings: 1,
            static_slots: 1,
        }
    }

    fn jmp(target: u32) -> Byte {
        Byte::new(Instruction::JMP).with_operand_u32(target)
    }

    #[test]
    fn accepts_well_formed_program() {
        let code = [
            jmp(2),
            Byte::new(Instruction::CONST).with_const_pool(0),
            Byte::new(Instruction::STRING).with_operand_u32(0),
            Byte::new(Instruction::CALL).with_call_packed(0, 3),
            jmp(5),
        ];
        assert_eq!(verify_bytecode(&code, &[7], limits()), Ok(()));
    }

    #[test]
    fn rejects_out_of_range_operands() {
        let cases = [
            jmp(9),
            Byte::new(Instruction::CONST).with_const_pool(4),
            Byte::new(Instruction::STRING).with_operand_u32(3),
            Byte::new(Instruction::LoadStatic).with_operand_u32(1),
            Byte::new(Instruction::CALL).with_call_packed(0, 1),
            Byte::new(Instruction::CmpJmpf).with_cmp_jmpf_pool(0, 2),
        ];
        for bad in cases {
            let err = verify_bytecode(&[bad], &[7], limits()).unwrap_err();
            assert_eq!(err.pc, 0, "{err}");
        }
    }

    #[test]
    fn rejects_pool_jump_descriptor_past_end() {
        let code = [Byte::new(Instruction::BinSlotSlotJmpf).with_bin_slot_slot_jmpf(0, 0, 0)];
        assert!(verify_bytecode(&code, &[5u64 << 32], limits()).is_err());
        assert!(verify_bytecode(&code, &[1u64 << 32], limits()).is_ok());
    }

    #[test]
    fn rejects_retired_opcodes() {
        let code = [Byte::new(Instruction::ReturnPair)];
        let err = verify_bytecode(&code, &[], limits()).unwrap_err();
        assert!(err.reason.contains("retired"), "{err}");
    }
}
