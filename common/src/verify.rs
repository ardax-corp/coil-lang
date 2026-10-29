//! Load-time bytecode verification.
//!
//! The VM decodes operands with `promise!` + unchecked reads, so a corrupt or
//! hand-edited archive would be undefined behaviour rather than an error.
//! [`verify_bytecode`] checks every operand the interpreter trusts without a
//! runtime test: retired opcodes, jump / call targets, constant-pool,
//! string-table and static-slot indices, and the inner opcode byte fused
//! ops carry (the VM turns it back into an `Instruction` by transmute).
//! Frame-slot operands and stack height are still compiler invariants (they
//! need per-function frame sizes).

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
        // A fused op's inner opcode byte becomes an `Instruction` by an
        // unchecked transmute in the VM; past the last variant that is UB.
        let inner = |code: u8, what: &str| {
            if code > Instruction::LAST as u8 {
                fail(format!("{what} inner opcode {code} out of range"))
            } else {
                Ok(())
            }
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
            RestoreSnapshot => {
                let snapshot = constants
                    .get(operand as usize..)
                    .ok_or(crate::SnapshotDecodeError::Truncated)
                    .and_then(crate::HeapSnapshot::decode);
                match snapshot {
                    Err(e) => return fail(format!("RestoreSnapshot at pool index {operand}: {e}")),
                    Ok(s) => {
                        if let Some((slot, _)) = s.statics.iter().find(|(slot, _)| *slot as usize >= limits.static_slots) {
                            return fail(format!(
                                "RestoreSnapshot static slot {slot} out of range ({})",
                                limits.static_slots
                            ));
                        }
                    }
                }
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
            BinSlotImm => inner(byte.bin_slot_imm_parts().0, "BinSlotImm")?,
            BinSlotSlot => inner(byte.bin_slot_slot_parts().0, "BinSlotSlot")?,
            BinSlotSlotStore => inner(byte.bin_slot_slot_store_parts().0, "BinSlotSlotStore")?,
            BinReturn => inner(byte.bin_return_op(), "BinReturn")?,
            CmpJmpf | CmpJmpt => {
                let (cmp, t) = byte.cmp_jmpf_parts();
                inner(cmp, "CmpJmp")?;
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
                let (cmp, _, idx) = byte.bin_slot_imm_jmpf_parts();
                inner(cmp, "BinSlotImmJmp")?;
                let desc = pool(idx, "BinSlotImmJmp")?;
                jump((desc >> 32) as usize, "BinSlotImmJmp")?;
            }
            BinSlotSlotJmpf | BinSlotSlotJmpt => {
                let (cmp, _, idx) = byte.bin_slot_slot_jmpf_parts();
                inner(cmp, "BinSlotSlotJmp")?;
                let desc = pool(idx, "BinSlotSlotJmp")?;
                jump((desc >> 32) as usize, "BinSlotSlotJmp")?;
            }
            BinSlotSlotConstJmpt => {
                let desc = pool(byte.bin_slot_slot_const_jmpf_parts().2, "BinSlotSlotConstJmp")?;
                let (_, _, float_idx, target) = Byte::unpack_bin_slot_slot_const_jmpf_desc(desc);
                pool(float_idx, "BinSlotSlotConstJmp float")?;
                jump(target, "BinSlotSlotConstJmp")?;
            }
            BinSlotImmStore => {
                let (bin, _, idx) = byte.bin_slot_imm_store_parts();
                inner(bin, "BinSlotImmStore")?;
                pool(idx, "BinSlotImmStore")?;
            }
            DenseMakeK => {
                pool((operand & 0xFFFF) as usize, "DenseMakeK")?;
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

    /// `RestoreSnapshot` must point at a whole snapshot whose slots exist.
    #[test]
    fn restore_snapshot_needs_a_valid_snapshot_and_slots() {
        use crate::{HeapSnapshot, SnapWord};
        let code = [Byte::new(Instruction::RestoreSnapshot).with_operand_u32(1), Byte::new(Instruction::HALT)];
        let snapshot = |slot| HeapSnapshot { objects: vec![], statics: vec![(slot, SnapWord::Raw(1))] };
        let mut pool = vec![7];
        pool.extend(snapshot(0).encode());
        assert!(verify_bytecode(&code, &pool, limits()).is_ok());
        let mut pool = vec![7];
        pool.extend(snapshot(1).encode());
        let err = verify_bytecode(&code, &pool, limits()).expect_err("slot 1 of 1");
        assert!(err.reason.contains("static slot 1"), "{}", err.reason);
        let mut pool = vec![7];
        pool.extend(&snapshot(0).encode()[..3]);
        assert!(verify_bytecode(&code, &pool, limits()).is_err(), "truncated");
        assert!(verify_bytecode(&code, &[7], limits()).is_err(), "past the pool");
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

    /// A fused op's inner opcode byte past the last `Instruction` would be
    /// transmuted into an invalid enum value by the VM.
    #[test]
    fn rejects_fused_inner_opcode_out_of_range() {
        let add = Instruction::ADD as u8;
        let lt = Instruction::LE as u8;
        let last = Instruction::LAST as u8;
        let fused = |op: u8, cmp: u8| {
            [
                Byte::new(Instruction::BinSlotImm).with_bin_slot_imm(op, 0, 1),
                Byte::new(Instruction::BinSlotSlot).with_bin_slot_slot(op, 0, 0),
                Byte::new(Instruction::BinSlotSlotStore).with_bin_slot_slot_store(op, 0, 0, 0),
                Byte::new(Instruction::BinSlotImmStore).with_bin_slot_imm_store(op, 0, 0),
                Byte::new(Instruction::BinReturn).with_bin_return(op),
                Byte::new(Instruction::CmpJmpf).with_cmp_jmpf(cmp, 0),
                Byte::new(Instruction::CmpJmpt).with_cmp_jmpf(cmp, 0),
                Byte::new(Instruction::BinSlotImmJmpf).with_bin_slot_imm_jmpf(cmp, 0, 0),
                Byte::new(Instruction::BinSlotImmJmpt).with_bin_slot_imm_jmpf(cmp, 0, 0),
                Byte::new(Instruction::BinSlotSlotJmpf).with_bin_slot_slot_jmpf(cmp, 0, 0),
                Byte::new(Instruction::BinSlotSlotJmpt).with_bin_slot_slot_jmpf(cmp, 0, 0),
            ]
        };
        // Pool word 0: jump descriptor with target 0, also a valid imm.
        let pool = [0u64];
        for good in fused(add, lt).into_iter().chain(fused(last, last)) {
            let res = verify_bytecode(&[good], &pool, limits());
            assert_eq!(res, Ok(()), "{:#010x}", good.operand_u32());
        }
        for bad in fused(last + 1, last + 1).into_iter().chain(fused(0xFF, 0xFF)) {
            let err = verify_bytecode(&[bad], &pool, limits()).unwrap_err();
            assert_eq!(err.pc, 0, "{err}");
            assert!(err.reason.contains("inner opcode"), "{err}");
        }
    }

    #[test]
    fn rejects_retired_opcodes() {
        let code = [Byte::new(Instruction::ReturnPair)];
        let err = verify_bytecode(&code, &[], limits()).unwrap_err();
        assert!(err.reason.contains("retired"), "{err}");
    }
}
