//! Operand-height (SP) analysis over IL ops.
//!
//! This is **not** the shared buffer cursor: [`super::tell`] tracks `tell`,
//! which `STORE` floors to `slot + 1` even when height is lower. `sp` only
//! counts eval-stack values. Nested `CALL`/`MakeCoro` reset height to 1
//! (return value only), not `before + (1 - arity)`.
//!
//! Used by return-join convoys, fuse/canon, and mem_fwd. Do not substitute
//! tell here — a STORE floor is not height (COI-81).

use common::Instruction;

use super::op::{EntryKind, IlJumpKind, IlOp, Label};

/// Stack height relative to analysis entry (usually 0 at `ops[0]`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Sp {
    Known(i32),
    Unknown,
}

impl Sp {
    pub fn is_known(self) -> bool {
        matches!(self, Sp::Known(_))
    }

    pub fn known(self) -> Option<i32> {
        match self {
            Sp::Known(v) => Some(v),
            Sp::Unknown => None,
        }
    }

    fn apply(self, delta: Option<i32>) -> Sp {
        match (self, delta) {
            (Sp::Known(h), Some(d)) => Sp::Known(h + d),
            _ => Sp::Unknown,
        }
    }
}

/// Per-op SP-in (height before the op at that index).
#[derive(Clone, Debug)]
pub struct SpInfo {
    pub sp_in: Vec<Sp>,
}

impl SpInfo {
    pub fn sp_before(&self, idx: usize) -> Sp {
        self.sp_in.get(idx).copied().unwrap_or(Sp::Unknown)
    }
}

/// Net stack delta for `op`, or `None` if the effect is unknown / fail-closed.
pub fn stack_delta(op: &IlOp) -> Option<i32> {
    match op {
        IlOp::Label(_) | IlOp::JoinLabel(_) => Some(0),
        IlOp::Load { .. }
        | IlOp::Const { .. }
        | IlOp::ConstPool { .. }
        | IlOp::String { .. }
        | IlOp::Dup { .. } => Some(1),
        IlOp::LogNot { .. } => Some(0),
        IlOp::StorePop { .. } | IlOp::Pop { .. } | IlOp::ArrayPin { .. } => Some(-1),
        IlOp::Index { .. } | IlOp::IndexUnchecked { .. } => Some(-1),
        IlOp::IndexPin { .. } | IlOp::IndexPinUnchecked { .. } => Some(0),
        IlOp::StoreIndexPin { .. } | IlOp::StoreIndexPinUnchecked { .. } => Some(-1),
        IlOp::MakeTuple { arity, .. } | IlOp::MakeArray { arity, .. } => Some(1 - *arity as i32),
        IlOp::MakeEnum { arity, .. } => Some(1 - *arity as i32),
        IlOp::BoxValue { .. } | IlOp::UnboxValue { .. } | IlOp::LoadField { .. } => Some(0),
        // GetField: pop target+name, push value (−1).
        // SetField: name form −2; slot form (no name) −1.
        IlOp::GetField { .. } => Some(-1),
        IlOp::SetField { index: Some(_), .. } => Some(-1),
        IlOp::SetField { index: None, .. } => Some(-2),
        // HostInvoke: pop fn_id + arity args, push result (delta −arity).
        IlOp::HostInvoke { arity, .. } => Some(-(*arity as i32)),
        IlOp::Print { .. } => Some(-1),
        IlOp::Bin { .. } => Some(-1),
        // Slot forms push a computed value without consuming eval-stack args.
        IlOp::BinSlotImm { .. } | IlOp::BinSlotSlot { .. } => Some(1),
        // Terminators: treat as consuming the returned value(s) for fall-through SP.
        IlOp::Return { ret_words, .. } => Some(-(*ret_words as i32)),
        IlOp::LoadReturnSlot { .. } | IlOp::ConstReturnImm { .. } => Some(-1),
        IlOp::BinReturn { .. } => Some(-2),
        IlOp::Halt { .. } => Some(0),
        IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            ..
        } => Some(0),
        IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse | IlJumpKind::JumpIfTrue,
            ..
        } => Some(-1),
        IlOp::Jump {
            kind: IlJumpKind::JumpIfMatch { arity, .. },
            ..
        } => {
            // Scrutinee consumed; arity payloads left in slots — eval stack −1.
            let _ = arity;
            Some(-1)
        }
        IlOp::Entry {
            kind: EntryKind::Call | EntryKind::MakeCoro,
            arity,
            ret_words,
            .. } => Some(*ret_words as i32 - *arity as i32),
        IlOp::Entry {
            kind: EntryKind::TailCall,
            .. } => None,
        IlOp::Entry {
            kind: EntryKind::CodePtr | EntryKind::MakePolyFn,
            .. } => Some(1),
        IlOp::PrologueJmp { .. } => Some(0),
        IlOp::Byte { byte, .. } => byte_stack_delta(*byte.bytecode(), byte),
    }
}

pub(super) fn byte_stack_delta(insn: Instruction, byte: &common::Byte) -> Option<i32> {
    match insn {
        Instruction::LOAD
        | Instruction::CONST
        | Instruction::DUPLICATE
        | Instruction::STRING
        | Instruction::CodePtr
        | Instruction::MakePolyFn => {
            if insn == Instruction::LOAD {
                Some(byte.load_store_count() as i32)
            } else {
                Some(1)
            }
        }
        Instruction::POP | Instruction::StorePop | Instruction::STORE => {
            if matches!(insn, Instruction::STORE | Instruction::StorePop) {
                Some(-(byte.load_store_count() as i32))
            } else {
                Some(-1)
            }
        }
        // Absolute cursor set — fail-closed for relative SP analysis.
        Instruction::Seek => None,
        Instruction::ADD
        | Instruction::SUB
        | Instruction::MUL
        | Instruction::DIV
        | Instruction::MOD
        | Instruction::LE
        | Instruction::LEQ
        | Instruction::GT
        | Instruction::GEQ
        | Instruction::EQ
        | Instruction::NEQ
        | Instruction::Pow
        | Instruction::BITAND
        | Instruction::BITOR
        | Instruction::ADDF
        | Instruction::SUBF
        | Instruction::MULF
        | Instruction::DIVF
        | Instruction::MODF
        | Instruction::LEF
        | Instruction::LEQF
        | Instruction::GTF
        | Instruction::GEQF
        | Instruction::PowF
        | Instruction::SHL
        | Instruction::SHR
        | Instruction::XOR
        | Instruction::AND
        | Instruction::OR => Some(-1),
        Instruction::NOT | Instruction::NEG | Instruction::NEGF => Some(0),
        Instruction::INIT | Instruction::InitTyped => Some(1),
        Instruction::BinSlotImm | Instruction::BinSlotSlot => Some(1),
        Instruction::BinSlotImmJmpf
        | Instruction::BinSlotImmJmpt
        | Instruction::BinSlotSlotJmpf
        | Instruction::BinSlotSlotJmpt
        | Instruction::BinSlotSlotConstJmpf
        | Instruction::BinSlotSlotConstJmpt
        | Instruction::BinSlotImmStore
        | Instruction::BinSlotSlotStore => Some(0),
        Instruction::CmpJmpf | Instruction::CmpJmpt => Some(-2),
        Instruction::LogNotJmpf | Instruction::LogNotJmpt => Some(-1),
        Instruction::RETURN => Some(-(byte.return_words() as i32)),
        Instruction::LoadReturnSlot | Instruction::ConstReturnImm => Some(-1),
        Instruction::ReturnPair => Some(-2),
        Instruction::BinReturn => Some(-2),
        Instruction::HALT | Instruction::NOOP => Some(0),
        Instruction::JMP => Some(0),
        Instruction::JMPF | Instruction::JMPT => Some(-1),
        Instruction::Index | Instruction::IndexUnchecked => Some(-1),
        Instruction::ArrayPin => Some(-1),
        Instruction::IndexPin | Instruction::IndexPinUnchecked => Some(0),
        Instruction::StoreIndexPin | Instruction::StoreIndexPinUnchecked => Some(-1),
        Instruction::BoxValue | Instruction::UnboxValue | Instruction::LoadField => Some(0),
        Instruction::OptionNicheToHeap | Instruction::HeapOptionToNiche => Some(0),
        Instruction::PairToHeap => Some(-1),
        Instruction::HeapToPair => Some(1),
        Instruction::CastIntToFloat
        | Instruction::CastFloatToInt
        | Instruction::CastIntToByte
        | Instruction::CastByteToInt
        | Instruction::CastIntToBool
        | Instruction::CastBoolToInt => Some(0),
        Instruction::MakeTuple | Instruction::MakeArray => Some(1 - byte.operand_u32() as i32),
        Instruction::MakeDict => {
            let arity = (byte.operand_u32() & 0xFFFF) as i32;
            Some(1 - 2 * arity)
        }
        Instruction::MakeEnum => Some(1 - byte.operand_u16(1) as i32),
        Instruction::CALL => {
            let (arity, _) = byte.call_parts();
            Some(byte.call_ret_words() as i32 - arity as i32)
        }
        Instruction::MakeCoro => {
            let (arity, _) = byte.call_parts();
            Some(1 - arity as i32)
        }
        Instruction::TailCall => None,
        Instruction::HostInvoke => Some(-((byte.operand_u32() & 0xFFFF) as i32)),
        Instruction::HostInvokeNiche => Some(-1),
        Instruction::FloatChainStore => Some(0),
        Instruction::PRINT | Instruction::GetField => Some(-1),
        Instruction::SetField => {
            if common::set_field_slot_index(byte.operand_u32()).is_some() {
                Some(-1)
            } else {
                Some(-2)
            }
        }
        // STRING pushes the ObjString; DATA is a stack-neutral archive tombstone.
        Instruction::DATA => Some(0),
        // FORMAT n: pop n args + format string, push result (−n). n==0 is a no-op.
        Instruction::FORMAT => {
            let n = byte.operand_u32() as i32;
            if n == 0 { Some(0) } else { Some(-n) }
        }
        // STRINGIFY: pop value, push string (net 0).
        Instruction::STRINGIFY => Some(0),
        // ArrayLen: pop array, push length (net 0).
        Instruction::ArrayLen => Some(0),
        // StoreIndex: pop value+index+target, push value (−2).
        Instruction::StoreIndex | Instruction::StoreIndexUnchecked => Some(-2),
        // ArrayPush: pop value+array, push array (−1).
        Instruction::ArrayPush => Some(-1),
        // Fail closed for the remaining long tail (FFI, …).
        _ => None,
    }
}

fn is_terminator(op: &IlOp) -> bool {
    matches!(
        op,
        IlOp::Return { .. }
            | IlOp::Halt { .. }
            | IlOp::LoadReturnSlot { .. }
            | IlOp::ConstReturnImm { .. }
            | IlOp::BinReturn { .. }
            | IlOp::Entry {
                kind: EntryKind::TailCall,
                .. }
    ) || matches!(
        op.as_encode_byte(),
        Some(b) if matches!(
            *b.bytecode(),
            Instruction::RETURN
                | Instruction::ReturnPair
                | Instruction::HALT
                | Instruction::LoadReturnSlot
                | Instruction::ConstReturnImm
                | Instruction::BinReturn
                | Instruction::TailCall
        )
    )
}

/// When `op` is a nested direct call, the VM return resets tell to
/// `frame_base + ret_words` (`1` boxed word, or `2` for a known ≤2-word
/// direct `CALL`; `MakeCoro` is always `1`).
fn nested_call_return_words(op: &IlOp) -> Option<i32> {
    match op {
        IlOp::Entry {
            kind: EntryKind::Call,
            ret_words,
            ..
        } => Some(*ret_words as i32),
        IlOp::Entry {
            kind: EntryKind::MakeCoro,
            ..
        } => Some(1),
        _ => match op.as_plain_byte() {
            Some(b) if *b.bytecode() == Instruction::CALL => Some(b.call_ret_words() as i32),
            Some(b) if *b.bytecode() == Instruction::MakeCoro => Some(1),
            _ => None,
        },
    }
}

/// Compute SP-in for each op. Entry SP is 0 at index 0; unknown effects poison.
pub fn analyze(ops: &[IlOp]) -> SpInfo {
    analyze_at(ops, 0)
}

/// Like [`analyze`], but seed height at `entry_sp` (function arity / frame base).
///
/// Per-function bodies begin with args already on the shared locals/operand
/// stack; starting at 0 understates height and lets `mem_fwd` emit `Dup;Store`
/// that aliases a local with TOS.
pub fn analyze_at(ops: &[IlOp], entry_sp: i32) -> SpInfo {
    let n = ops.len();
    let mut sp_in: Vec<Option<Sp>> = vec![None; n];
    if n == 0 {
        return SpInfo { sp_in: Vec::new() };
    }
    sp_in[0] = Some(Sp::Known(entry_sp));

    let mut label_at: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
    for (i, op) in ops.iter().enumerate() {
        if let IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) = op {
            label_at.insert(*id, i);
        }
    }

    fn meet_into(slot: &mut Option<Sp>, incoming: Sp) -> bool {
        let next = match *slot {
            None => incoming,
            Some(Sp::Unknown) => Sp::Unknown,
            Some(Sp::Known(a)) => match incoming {
                Sp::Known(b) if a == b => Sp::Known(a),
                _ => Sp::Unknown,
            },
        };
        if *slot != Some(next) {
            *slot = Some(next);
            true
        } else {
            false
        }
    }

    for _ in 0..n.saturating_mul(2).max(8) {
        let mut changed = false;
        // `None` = no fall-through edge into the next index.
        let mut fall_sp: Option<Sp> = Some(Sp::Known(entry_sp));
        for i in 0..n {
            if i > 0
                && let Some(edge) = fall_sp
            {
                changed |= meet_into(&mut sp_in[i], edge);
            }

            let op = &ops[i];
            let before = sp_in[i].unwrap_or(Sp::Unknown);
            // Nested CALL/MakeCoro return seeks to frame_base and pushes one
            // result → relative height is always 1, not `before + (1 - arity)`.
            // Modeling the arithmetic delta lets mem_fwd emit Dup;Store that
            // later operand pops destroy (http parse_url / bytes_slice hang).
            let after = if let Some(ret_words) = nested_call_return_words(op) {
                Sp::Known(ret_words)
            } else {
                before.apply(stack_delta(op))
            };

            if let IlOp::Jump { kind, target, .. } = op {
                if let Some(&t) = label_at.get(&target.0) {
                    let edge_sp = match kind {
                        IlJumpKind::Unconditional => before.apply(Some(0)),
                        IlJumpKind::JumpIfFalse | IlJumpKind::JumpIfTrue => before.apply(Some(-1)),
                        IlJumpKind::JumpIfMatch { .. } => before.apply(Some(-1)),
                    };
                    changed |= meet_into(&mut sp_in[t], edge_sp);
                }
                fall_sp = match kind {
                    IlJumpKind::Unconditional => None,
                    _ => Some(after),
                };
            } else if is_terminator(op) {
                fall_sp = None;
            } else if matches!(op, IlOp::Label(_) | IlOp::JoinLabel(_)) {
                fall_sp = Some(before);
            } else {
                fall_sp = Some(after);
            }
        }
        if !changed {
            break;
        }
    }

    SpInfo {
        sp_in: sp_in
            .into_iter()
            .map(|s| s.unwrap_or(Sp::Unknown))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::DebugLoc;

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

    #[test]
    fn straight_line_known_heights() {
        let ops = vec![
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Const { imm: 2, loc: loc() },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        let info = analyze(&ops);
        assert_eq!(info.sp_before(0), Sp::Known(0));
        assert_eq!(info.sp_before(1), Sp::Known(1));
        assert_eq!(info.sp_before(2), Sp::Known(2));
        assert_eq!(info.sp_before(3), Sp::Known(1));
    }

    #[test]
    fn diamond_agreeing_heights() {
        // CONST 0; JMPF Lelse; CONST 1; JMP Ljoin; Label Lelse; CONST 2; Label Ljoin; RETURN
        let ops = vec![
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(2),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
            IlOp::Const { imm: 2, loc: loc() },
            IlOp::Label(Label(2)),
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        let info = analyze(&ops);
        // After JMPF, SP is 0 on both arms; each CONST → 1 at join.
        assert_eq!(info.sp_before(6), Sp::Known(1)); // Label join
        assert_eq!(info.sp_before(7), Sp::Known(1)); // RETURN
    }

    #[test]
    fn diamond_mismatched_heights_unknown_at_join() {
        // Then-arm pushes two consts; else pushes one — join SP disagrees.
        let ops = vec![
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Const { imm: 3, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(2),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
            IlOp::Const { imm: 2, loc: loc() },
            IlOp::Label(Label(2)),
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        let info = analyze(&ops);
        assert_eq!(info.sp_before(7), Sp::Unknown);
    }

    #[test]
    fn format_and_stringify_stack_deltas() {
        use common::Byte;
        let format1 = IlOp::byte(Byte::new(Instruction::FORMAT).with_operand_u32(1));
        let format0 = IlOp::byte(Byte::new(Instruction::FORMAT).with_operand_u32(0));
        let stringify = IlOp::byte(Byte::new(Instruction::STRINGIFY));
        assert_eq!(stack_delta(&format1), Some(-1));
        assert_eq!(stack_delta(&format0), Some(0));
        assert_eq!(stack_delta(&stringify), Some(0));

        // FORMAT 1: args+fmt on stack → result; SP stays Known through PRINT.
        let ops = vec![
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::byte(Byte::new(Instruction::STRING).with_operand_u32(0)),
            IlOp::byte(Byte::new(Instruction::FORMAT).with_operand_u32(1)),
            IlOp::Print { loc: loc() },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        let info = analyze(&ops);
        assert_eq!(info.sp_before(0), Sp::Known(0));
        assert_eq!(info.sp_before(2), Sp::Known(2));
        assert_eq!(info.sp_before(3), Sp::Known(1));
        assert_eq!(info.sp_before(4), Sp::Known(0));
    }

    #[test]
    fn array_len_and_store_index_stack_deltas() {
        use common::Byte;
        assert_eq!(
            stack_delta(&IlOp::byte(Byte::new(Instruction::ArrayLen))),
            Some(0)
        );
        assert_eq!(
            stack_delta(&IlOp::byte(Byte::new(Instruction::StoreIndex))),
            Some(-2)
        );
        assert_eq!(
            stack_delta(&IlOp::byte(Byte::new(Instruction::ArrayPush))),
            Some(-1)
        );
    }

    #[test]
    fn stack_delta_bin_slot_forms_push_one() {
        let imm = IlOp::BinSlotImm {
            op: Instruction::ADD as u8,
            slot: 0,
            imm: 1,
            loc: loc(),
        };
        let slot = IlOp::BinSlotSlot {
            op: Instruction::ADD as u8,
            a: 0,
            b: 1,
            loc: loc(),
        };
        assert_eq!(stack_delta(&imm), Some(1));
        assert_eq!(stack_delta(&slot), Some(1));
    }

    /// Packed LOAD/STORE and fused assign/jmpf forms must report accurate net deltas
    /// so convoy/LICM SP analysis does not poison on the new encodings.
    #[test]
    fn stack_delta_packed_load_store_and_fused_stores() {
        use common::Byte;
        let load3 = IlOp::byte(Byte::new(Instruction::LOAD).with_load_store_packed(3, 0, 1, 2));
        let store2 = IlOp::byte(Byte::new(Instruction::STORE).with_load_store_packed(2, 4, 5, 0));
        let imm_store = IlOp::byte(
            Byte::new(Instruction::BinSlotImmStore).with_bin_slot_imm_store(
                Instruction::ADD as u8,
                0,
                1,
            ),
        );
        let slot_store = IlOp::byte(
            Byte::new(Instruction::BinSlotSlotStore).with_bin_slot_slot_store(
                Instruction::BITAND as u8,
                0,
                1,
                2,
            ),
        );
        let slot_jmpf = IlOp::byte(
            Byte::new(Instruction::BinSlotSlotJmpf).with_bin_slot_slot_jmpf(
                Instruction::AND as u8,
                0,
                3,
            ),
        );
        assert_eq!(stack_delta(&load3), Some(3));
        assert_eq!(stack_delta(&store2), Some(-2));
        assert_eq!(stack_delta(&imm_store), Some(0));
        assert_eq!(stack_delta(&slot_store), Some(0));
        assert_eq!(stack_delta(&slot_jmpf), Some(0));
    }

    #[test]
    fn stack_delta_call_uses_arity_tail_call_unknown() {
        let call = IlOp::Entry {
            kind: EntryKind::Call,
            arity: 2,
            target: Label(0),
            loc: loc(), ret_words: 1,};
        let tail = IlOp::Entry {
            kind: EntryKind::TailCall,
            arity: 1,
            target: Label(0),
            loc: loc(), ret_words: 1,};
        assert_eq!(stack_delta(&call), Some(-1));
        assert_eq!(stack_delta(&tail), None);
    }

    /// `1 - arity`, not JumpIfMatch's `arity - 1`. Arity 0 is the discriminator
    /// (`+1` vs `-1`); arity 1 agrees under both formulas. Corpus gate: COI-80
    /// `tell_symbolic_il_entry_call_delta_is_not_jump_if_match_arity_minus_one`.
    #[test]
    fn stack_delta_call_uses_one_minus_arity_not_arity_minus_one() {
        let call0 = IlOp::Entry {
            kind: EntryKind::Call,
            arity: 0,
            target: Label(0),
            loc: loc(), ret_words: 1,};
        let coro0 = IlOp::Entry {
            kind: EntryKind::MakeCoro,
            arity: 0,
            target: Label(0),
            loc: loc(), ret_words: 1,};
        assert_eq!(stack_delta(&call0), Some(1));
        assert_eq!(stack_delta(&coro0), Some(1));
        assert_ne!(
            stack_delta(&call0),
            Some(-1),
            "must not use JumpIfMatch's arity-1"
        );
    }

    /// STORE pops; it does not raise operand height to `slot + 1`. After
    /// `CONST; STORE 5` height is 0, not 6 — that floor is `il::tell`.
    #[test]
    fn store_does_not_raise_operand_height() {
        let ops = vec![
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::StorePop {
                slot: 5,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        let info = analyze(&ops);
        assert_eq!(info.sp_before(0), Sp::Known(0));
        assert_eq!(info.sp_before(1), Sp::Known(1));
        assert_eq!(info.sp_before(2), Sp::Known(0));
        assert_eq!(stack_delta(&ops[1]), Some(-1));
    }

    #[test]
    fn stack_delta_jump_if_match_consumes_scrutinee() {
        let jmp = IlOp::Jump {
            kind: IlJumpKind::JumpIfMatch { tag: 1, arity: 2 },
            target: Label(0),
            loc: loc(),
            hint: Default::default(),
        };
        assert_eq!(stack_delta(&jmp), Some(-1));
    }

    #[test]
    fn stack_delta_typed_longtail_ops() {
        assert_eq!(stack_delta(&IlOp::Index { loc: loc() }), Some(-1));
        assert_eq!(
            stack_delta(&IlOp::MakeTuple {
                arity: 3,
                loc: loc(),
            }),
            Some(-2)
        );
        assert_eq!(
            stack_delta(&IlOp::MakeArray {
                arity: 2,
                loc: loc(),
            }),
            Some(-1)
        );
        assert_eq!(
            stack_delta(&IlOp::MakeEnum {
                tag: 1,
                arity: 0,
                loc: loc(),
            }),
            Some(1)
        );
        assert_eq!(
            stack_delta(&IlOp::MakeEnum {
                tag: 1,
                arity: 2,
                loc: loc(),
            }),
            Some(-1)
        );
        assert_eq!(stack_delta(&IlOp::BoxValue { tag: 0, loc: loc() }), Some(0));
        assert_eq!(
            stack_delta(&IlOp::UnboxValue { tag: 0, loc: loc() }),
            Some(0)
        );
        assert_eq!(
            stack_delta(&IlOp::LoadField {
                index: 1,
                loc: loc(),
            }),
            Some(0)
        );
        assert_eq!(stack_delta(&IlOp::GetField { loc: loc() }), Some(-1));
        assert_eq!(stack_delta(&IlOp::SetField { loc: loc(), index: None }), Some(-2));
        assert_eq!(
            stack_delta(&IlOp::SetField {
                loc: loc(),
                index: Some(1)
            }),
            Some(-1)
        );
        assert_eq!(
            stack_delta(&IlOp::HostInvoke {
                arity: 3,
                loc: loc(),
            }),
            Some(-3)
        );
        assert_eq!(stack_delta(&IlOp::Print { loc: loc() }), Some(-1));
        assert_eq!(
            stack_delta(&IlOp::ConstPool { idx: 2, loc: loc() }),
            Some(1)
        );
        assert_eq!(stack_delta(&IlOp::String { idx: 4, loc: loc() }), Some(1));
    }

    #[test]
    fn make_dict_and_data_stack_deltas() {
        use common::Byte;
        // MakeDict arity=2: pop 4 (k,v,k,v), push dict → −3.
        assert_eq!(
            stack_delta(&IlOp::byte(
                Byte::new(Instruction::MakeDict).with_operand_u32(2)
            )),
            Some(1 - 2 * 2)
        );
        assert_eq!(
            stack_delta(&IlOp::byte(
                Byte::new(Instruction::MakeDict).with_operand_u32(0)
            )),
            Some(1)
        );
        // DATA is a legacy archive tombstone (net 0).
        assert_eq!(
            stack_delta(&IlOp::byte(
                Byte::new(Instruction::DATA).with_operand_u32(b'a' as u32)
            )),
            Some(0)
        );
        let ops = vec![
            IlOp::byte(Byte::new(Instruction::STRING).with_operand_u32(1)),
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        let info = analyze(&ops);
        assert_eq!(info.sp_before(0), Sp::Known(0));
        assert_eq!(info.sp_before(1), Sp::Known(1));
    }

    #[test]
    fn set_field_byte_matches_typed_delta() {
        use common::Byte;
        assert_eq!(
            stack_delta(&IlOp::byte(Byte::new(Instruction::SetField))),
            Some(-2)
        );
        assert_eq!(
            stack_delta(&IlOp::byte(Byte::new(Instruction::GetField))),
            Some(-1)
        );
    }

    #[test]
    fn call_entry_resets_height_to_one() {
        // Nested CALL returns with tell = frame_base + 1 regardless of arity
        // / pre-call height (VM seeks then pushes the result).
        let ops = vec![
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Const { imm: 2, loc: loc() },
            IlOp::Entry {
                kind: EntryKind::Call,
                arity: 2,
                target: Label(0),
                loc: loc(), ret_words: 1,},
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        let info = analyze(&ops);
        assert_eq!(info.sp_before(2), Sp::Known(2));
        assert_eq!(info.sp_before(3), Sp::Known(1));
    }

    /// COI-81: `MakeCoro` shares Call's absolute height reset (not relative delta).
    #[test]
    fn make_coro_resets_height_to_one_like_call() {
        let ops = vec![
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Const { imm: 2, loc: loc() },
            IlOp::Const { imm: 3, loc: loc() },
            IlOp::Entry {
                kind: EntryKind::MakeCoro,
                arity: 0,
                target: Label(0),
                loc: loc(), ret_words: 1,},
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        let info = analyze(&ops);
        assert_eq!(info.sp_before(3), Sp::Known(3));
        assert_eq!(info.sp_before(4), Sp::Known(1));
        assert_ne!(
            info.sp_before(4).known(),
            Some(4),
            "relative 1-arity would leave height 4"
        );
    }

    #[test]
    fn call_with_high_pre_height_still_returns_at_one() {
        // Arithmetic delta would be 1 - 0 = pre_height; absolute reset to 1.
        let ops = vec![
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Const { imm: 2, loc: loc() },
            IlOp::Const { imm: 3, loc: loc() },
            IlOp::Entry {
                kind: EntryKind::Call,
                arity: 0,
                target: Label(0),
                loc: loc(), ret_words: 1,},
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        let info = analyze(&ops);
        assert_eq!(info.sp_before(3), Sp::Known(3));
        assert_eq!(info.sp_before(4), Sp::Known(1));
    }

    #[test]
    fn tail_call_cuts_fall_through() {
        // TailCall is a terminator with unknown delta — no fall-through SP edge.
        let ops = vec![
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Entry {
                kind: EntryKind::TailCall,
                arity: 1,
                target: Label(0),
                loc: loc(), ret_words: 1,},
            IlOp::Label(Label(0)),
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        let info = analyze(&ops);
        assert_eq!(info.sp_before(1), Sp::Known(1));
        // Entry edges are not jump edges; label after TailCall has no SP meet.
        assert_eq!(info.sp_before(2), Sp::Unknown);
        assert_eq!(info.sp_before(3), Sp::Unknown);
    }
}
