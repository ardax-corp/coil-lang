//! Stack IL opcodes and symbolic labels.

use common::{Byte, DebugLoc, Instruction};

/// Opaque forward/back-edge target resolved once at lower time.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Label(pub u32);

impl Label {
    pub fn id(self) -> u32 {
        self.0
    }
}

/// Lowering annotation on a jump or label bind. Not a dummy opcode.
///
/// Fuse-select and invert-guard honor this; production `NOOP` / `DUP;POP`
/// barriers are gone (D3).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct FuseHint {
    /// Do not fuse this op with neighbors (pair-`?` / pair-match `EQ;JMPF`).
    pub nofuse: bool,
    pub join: JoinClass,
}

impl FuseHint {
    pub const fn nofuse_value_under_jmp() -> Self {
        Self {
            nofuse: true,
            join: JoinClass::ValueUnderJmp,
        }
    }

    pub const fn value_join() -> Self {
        Self {
            nofuse: false,
            join: JoinClass::Value,
        }
    }

    pub fn blocks_cmp_jmp_fuse(self) -> bool {
        self.nofuse || matches!(self.join, JoinClass::ValueUnderJmp)
    }

    pub fn is_value_join(self) -> bool {
        matches!(self.join, JoinClass::Value)
    }
}

/// Join class on a lowering annotation.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum JoinClass {
    #[default]
    None,
    /// Match / `?` join that carries a stacked value. Fuse must not pull a
    /// producer from one predecessor across this bind.
    Value,
    /// Guard jump whose compare sits under a still-live stacked value
    /// (pair tag still on stack after `EQ;JMPF`).
    ValueUnderJmp,
}

/// Control-flow jump kind (IL-level; packing happens in the lowerer).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum IlJumpKind {
    Unconditional,
    JumpIfFalse,
    /// Complete jump set (JMPT); constructed in opts/tests, matched in lower/sp.
    #[allow(dead_code)]
    JumpIfTrue,
    JumpIfMatch {
        tag: u32,
        arity: u32,
    },
}

/// Call-like entry that carries a symbolic label instead of a PC.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Call,
    TailCall,
    MakeCoro,
    CodePtr,
    MakePolyFn,
}

/// One IL instruction. Labels occupy no final bytecode slot.
///
/// Hot-path ops are first-class variants (lifted on absorb from `Byte`).
/// [`IlOp::Byte`] remains the escape hatch for the long tail.
#[derive(Clone, PartialEq, Eq)]
pub enum IlOp {
    /// Ordinary VM instruction whose operand is not a code pointer.
    /// Jump/call ops that still embed absolute PCs are accepted for
    /// transitional emit paths; prefer [`IlOp::Jump`] / [`IlOp::Entry`].
    Byte {
        byte: Byte,
        loc: DebugLoc,
    },
    Load {
        slot: u32,
        loc: DebugLoc,
    },
    StorePop {
        slot: u32,
        loc: DebugLoc,
    },
    /// Inline `CONST` only (non-negative; pool / high-bit forms use [`IlOp::ConstPool`]).
    Const {
        imm: i32,
        loc: DebugLoc,
    },
    /// Pool-backed `CONST` (`POOL_FLAG | idx`); also absorbs high-bit inline encodings.
    ConstPool {
        idx: u32,
        loc: DebugLoc,
    },
    /// Table-indexed `STRING` (archive `strings[idx]`); pure stack push.
    String {
        idx: u32,
        loc: DebugLoc,
    },
    Dup {
        loc: DebugLoc,
    },
    Pop {
        loc: DebugLoc,
    },
    /// `Index` — pop array + index, push element.
    Index {
        loc: DebugLoc,
    },
    /// Bounds-proofed `Index` (see [`Instruction::IndexUnchecked`]).
    IndexUnchecked {
        loc: DebugLoc,
    },
    /// Pin a stack-top array in the current frame (operand: source slot).
    ArrayPin {
        slot: u32,
        loc: DebugLoc,
    },
    /// Index via a pinned array (operand: pin slot); pops index only.
    IndexPin {
        slot: u32,
        loc: DebugLoc,
    },
    /// Bounds-proofed [`Instruction::IndexPin`].
    IndexPinUnchecked {
        slot: u32,
        loc: DebugLoc,
    },
    /// `StoreIndex` via a pinned array (operand: pin slot); pops value + index.
    StoreIndexPin {
        slot: u32,
        loc: DebugLoc,
    },
    /// Bounds-proofed [`Instruction::StoreIndexPin`].
    StoreIndexPinUnchecked {
        slot: u32,
        loc: DebugLoc,
    },
    /// `MakeTuple` — pop `arity` values, push tuple.
    MakeTuple {
        arity: u32,
        loc: DebugLoc,
    },
    /// `MakeArray` — pop `arity` values, push array.
    MakeArray {
        arity: u32,
        loc: DebugLoc,
    },
    /// `MakeEnum` — pop `arity` payloads, push enum with `tag`.
    MakeEnum {
        tag: u16,
        arity: u16,
        loc: DebugLoc,
    },
    BoxValue {
        tag: u32,
        loc: DebugLoc,
    },
    UnboxValue {
        tag: u32,
        loc: DebugLoc,
    },
    LoadField {
        index: u32,
        loc: DebugLoc,
    },
    /// Dict / class field read — pop target + name, push value.
    GetField {
        loc: DebugLoc,
    },
    /// Dict / class field write — pop value + target + name, push value.
    SetField {
        loc: DebugLoc,
    },
    /// Host native call — pop fn id + args tuple; push result (delta −1).
    HostInvoke {
        arity: u32,
        loc: DebugLoc,
    },
    /// Print TOS string (consume).
    Print {
        loc: DebugLoc,
    },
    Return {
        loc: DebugLoc,
    },
    Halt {
        loc: DebugLoc,
    },
    /// Plain int/float binop or comparison (stack operands).
    Bin {
        op: Instruction,
        loc: DebugLoc,
    },
    BinSlotImm {
        op: u8,
        slot: u8,
        imm: i16,
        loc: DebugLoc,
    },
    BinSlotSlot {
        op: u8,
        a: u8,
        b: u8,
        loc: DebugLoc,
    },
    LoadReturnSlot {
        slot: u32,
        loc: DebugLoc,
    },
    ConstReturnImm {
        imm: u32,
        loc: DebugLoc,
    },
    BinReturn {
        op: Instruction,
        loc: DebugLoc,
    },
    /// Bind `label` to the next emitting instruction's PC (last bind wins).
    Label(Label),
    /// Value-producing join bind (match / `?` end). Same PC rule as [`IlOp::Label`].
    JoinLabel(Label),
    /// Control-flow jump with a symbolic target.
    Jump {
        kind: IlJumpKind,
        target: Label,
        loc: DebugLoc,
        hint: FuseHint,
    },
    /// CALL / TailCall / MakeCoro / CodePtr / MakePolyFn with a label target.
    Entry {
        kind: EntryKind,
        arity: u32,
        target: Label,
        loc: DebugLoc,
    },
    /// Prologue JMP placeholder (`u32::MAX`); patched by the pipeline after lower.
    PrologueJmp {
        loc: DebugLoc,
    },
}

fn is_plain_bin_instruction(op: Instruction) -> bool {
    matches!(
        op,
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
            | Instruction::OR
    )
}

impl IlOp {
    pub fn byte(byte: Byte) -> Self {
        Self::from_plain_byte(byte, DebugLoc::unknown())
    }

    #[allow(dead_code)]
    pub fn byte_at(byte: Byte, loc: DebugLoc) -> Self {
        Self::from_plain_byte(byte, loc)
    }

    /// Lift a packed `Byte` into a typed hot-set variant when possible.
    pub fn from_plain_byte(byte: Byte, loc: DebugLoc) -> Self {
        match *byte.bytecode() {
            Instruction::LOAD => match byte.load_store_single_slot() {
                Some(slot) => Self::Load { slot, loc },
                None => Self::Byte { byte, loc },
            },
            Instruction::STORE | Instruction::StorePop => match byte.load_store_single_slot() {
                Some(slot) => Self::StorePop { slot, loc },
                None => Self::Byte { byte, loc },
            },
            Instruction::CONST => {
                let op = byte.operand_u32();
                if op & Byte::POOL_FLAG != 0 {
                    Self::ConstPool {
                        idx: op & !Byte::POOL_FLAG,
                        loc,
                    }
                } else {
                    Self::Const {
                        imm: op as i32,
                        loc,
                    }
                }
            }
            Instruction::STRING => Self::String {
                idx: byte.operand_u32(),
                loc,
            },
            Instruction::DUPLICATE => Self::Dup { loc },
            Instruction::POP => Self::Pop { loc },
            Instruction::Index => Self::Index { loc },
            Instruction::IndexUnchecked => Self::IndexUnchecked { loc },
            Instruction::ArrayPin => Self::ArrayPin {
                slot: byte.operand_u32(),
                loc,
            },
            Instruction::IndexPin => Self::IndexPin {
                slot: byte.operand_u32(),
                loc,
            },
            Instruction::IndexPinUnchecked => Self::IndexPinUnchecked {
                slot: byte.operand_u32(),
                loc,
            },
            Instruction::StoreIndexPin => Self::StoreIndexPin {
                slot: byte.operand_u32(),
                loc,
            },
            Instruction::StoreIndexPinUnchecked => Self::StoreIndexPinUnchecked {
                slot: byte.operand_u32(),
                loc,
            },
            Instruction::MakeTuple => Self::MakeTuple {
                arity: byte.operand_u32(),
                loc,
            },
            Instruction::MakeArray => Self::MakeArray {
                arity: byte.operand_u32(),
                loc,
            },
            Instruction::MakeEnum => Self::MakeEnum {
                tag: byte.operand_u16(0),
                arity: byte.operand_u16(1),
                loc,
            },
            Instruction::BoxValue => Self::BoxValue {
                tag: byte.operand_u32(),
                loc,
            },
            Instruction::UnboxValue => Self::UnboxValue {
                tag: byte.operand_u32(),
                loc,
            },
            Instruction::LoadField => Self::LoadField {
                index: byte.operand_u32(),
                loc,
            },
            Instruction::GetField => Self::GetField { loc },
            Instruction::SetField => Self::SetField { loc },
            Instruction::HostInvoke => Self::HostInvoke {
                arity: byte.operand_u32(),
                loc,
            },
            Instruction::PRINT => Self::Print { loc },
            Instruction::RETURN => Self::Return { loc },
            Instruction::HALT => Self::Halt { loc },
            Instruction::BinSlotImm => {
                let (op, slot, imm) = byte.bin_slot_imm_parts();
                Self::BinSlotImm {
                    op,
                    slot: slot as u8,
                    imm: imm as i16,
                    loc,
                }
            }
            Instruction::BinSlotSlot => {
                let (op, a, b) = byte.bin_slot_slot_parts();
                Self::BinSlotSlot {
                    op,
                    a: a as u8,
                    b: b as u8,
                    loc,
                }
            }
            Instruction::LoadReturnSlot => Self::LoadReturnSlot {
                slot: byte.operand_u32(),
                loc,
            },
            Instruction::ConstReturnImm => Self::ConstReturnImm {
                imm: byte.operand_u32(),
                loc,
            },
            Instruction::BinReturn => Self::BinReturn {
                op: byte.bin_return_op().into(),
                loc,
            },
            other if is_plain_bin_instruction(other) => Self::Bin { op: other, loc },
            _ => Self::Byte { byte, loc },
        }
    }

    /// Encode this op as a VM `Byte` for lower / round-trip. Control/label → `None`.
    pub fn as_encode_byte(&self) -> Option<Byte> {
        Some(match self {
            IlOp::Byte { byte, .. } => *byte,
            IlOp::Load { slot, .. } => Byte::new(Instruction::LOAD).with_load_store_slot(*slot),
            IlOp::StorePop { slot, .. } => {
                Byte::new(Instruction::STORE).with_load_store_slot(*slot)
            }
            IlOp::Const { imm, .. } => Byte::new(Instruction::CONST).with_const_inline(*imm),
            IlOp::ConstPool { idx, .. } => Byte::new(Instruction::CONST).with_const_pool(*idx),
            IlOp::String { idx, .. } => Byte::new(Instruction::STRING).with_operand_u32(*idx),
            IlOp::Dup { .. } => Byte::new(Instruction::DUPLICATE),
            IlOp::Pop { .. } => Byte::new(Instruction::POP),
            IlOp::Index { .. } => Byte::new(Instruction::Index),
            IlOp::IndexUnchecked { .. } => Byte::new(Instruction::IndexUnchecked),
            IlOp::ArrayPin { slot, .. } => Byte::new(Instruction::ArrayPin).with_operand_u32(*slot),
            IlOp::IndexPin { slot, .. } => Byte::new(Instruction::IndexPin).with_operand_u32(*slot),
            IlOp::IndexPinUnchecked { slot, .. } => {
                Byte::new(Instruction::IndexPinUnchecked).with_operand_u32(*slot)
            }
            IlOp::StoreIndexPin { slot, .. } => {
                Byte::new(Instruction::StoreIndexPin).with_operand_u32(*slot)
            }
            IlOp::StoreIndexPinUnchecked { slot, .. } => {
                Byte::new(Instruction::StoreIndexPinUnchecked).with_operand_u32(*slot)
            }
            IlOp::MakeTuple { arity, .. } => {
                Byte::new(Instruction::MakeTuple).with_operand_u32(*arity)
            }
            IlOp::MakeArray { arity, .. } => {
                Byte::new(Instruction::MakeArray).with_operand_u32(*arity)
            }
            IlOp::MakeEnum { tag, arity, .. } => {
                Byte::new(Instruction::MakeEnum).with_operands_u16([*tag, *arity])
            }
            IlOp::BoxValue { tag, .. } => Byte::new(Instruction::BoxValue).with_operand_u32(*tag),
            IlOp::UnboxValue { tag, .. } => {
                Byte::new(Instruction::UnboxValue).with_operand_u32(*tag)
            }
            IlOp::LoadField { index, .. } => {
                Byte::new(Instruction::LoadField).with_operand_u32(*index)
            }
            IlOp::GetField { .. } => Byte::new(Instruction::GetField),
            IlOp::SetField { .. } => Byte::new(Instruction::SetField),
            IlOp::HostInvoke { arity, .. } => {
                Byte::new(Instruction::HostInvoke).with_operand_u32(*arity)
            }
            IlOp::Print { .. } => Byte::new(Instruction::PRINT),
            IlOp::Return { .. } => Byte::new(Instruction::RETURN),
            IlOp::Halt { .. } => Byte::new(Instruction::HALT),
            IlOp::Bin { op, .. } => Byte::new(*op),
            IlOp::BinSlotImm { op, slot, imm, .. } => {
                Byte::new(Instruction::BinSlotImm).with_bin_slot_imm(*op, *slot, *imm)
            }
            IlOp::BinSlotSlot { op, a, b, .. } => {
                Byte::new(Instruction::BinSlotSlot).with_bin_slot_slot(*op, *a, *b)
            }
            IlOp::LoadReturnSlot { slot, .. } => {
                Byte::new(Instruction::LoadReturnSlot).with_operand_u32(*slot)
            }
            IlOp::ConstReturnImm { imm, .. } => {
                Byte::new(Instruction::ConstReturnImm).with_operand_u32(*imm)
            }
            IlOp::BinReturn { op, .. } => {
                Byte::new(Instruction::BinReturn).with_bin_return(*op as u8)
            }
            IlOp::Label(_)
            | IlOp::JoinLabel(_)
            | IlOp::Jump { .. }
            | IlOp::Entry { .. }
            | IlOp::PrologueJmp { .. } => {
                return None;
            }
        })
    }

    /// True if this op becomes one (or more) final bytecode slots.
    pub fn emits_code(&self) -> bool {
        !matches!(self, IlOp::Label(_) | IlOp::JoinLabel(_))
    }

    /// Bind site label id, if this is a label marker.
    pub fn bind_label(&self) -> Option<Label> {
        match self {
            IlOp::Label(id) | IlOp::JoinLabel(id) => Some(*id),
            _ => None,
        }
    }

    /// Lowering hint on this op (empty for everything except jumps and join labels).
    pub fn fuse_hint(&self) -> FuseHint {
        match self {
            IlOp::Jump { hint, .. } => *hint,
            IlOp::JoinLabel(_) => FuseHint::value_join(),
            _ => FuseHint::default(),
        }
    }

    pub fn jump(kind: IlJumpKind, target: Label, loc: DebugLoc) -> Self {
        IlOp::Jump {
            kind,
            target,
            loc,
            hint: FuseHint::default(),
        }
    }

    pub fn jump_hinted(kind: IlJumpKind, target: Label, loc: DebugLoc, hint: FuseHint) -> Self {
        IlOp::Jump {
            kind,
            target,
            loc,
            hint,
        }
    }

    /// Jump / Entry / PrologueJmp — not safe to copy as a tiny-inline body.
    pub fn is_control(&self) -> bool {
        matches!(
            self,
            IlOp::Jump { .. } | IlOp::Entry { .. } | IlOp::PrologueJmp { .. }
        )
    }

    /// Plain terminal `RETURN` (typed or residual `Byte`).
    ///
    /// Fused `*Return` variants are excluded — convoy opts and tiny-inline use
    /// this to find a real `RETURN` sink, not a fused return producer.
    pub fn is_plain_return(&self) -> bool {
        matches!(self, IlOp::Return { .. })
            || matches!(
                self,
                IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::RETURN
            )
    }

    pub fn loc(&self) -> DebugLoc {
        match self {
            IlOp::Byte { loc, .. }
            | IlOp::Load { loc, .. }
            | IlOp::StorePop { loc, .. }
            | IlOp::Const { loc, .. }
            | IlOp::ConstPool { loc, .. }
            | IlOp::String { loc, .. }
            | IlOp::Dup { loc }
            | IlOp::Pop { loc }
            | IlOp::Index { loc }
            | IlOp::IndexUnchecked { loc }
            | IlOp::ArrayPin { loc, .. }
            | IlOp::IndexPin { loc, .. }
            | IlOp::IndexPinUnchecked { loc, .. }
            | IlOp::StoreIndexPin { loc, .. }
            | IlOp::StoreIndexPinUnchecked { loc, .. }
            | IlOp::MakeTuple { loc, .. }
            | IlOp::MakeArray { loc, .. }
            | IlOp::MakeEnum { loc, .. }
            | IlOp::BoxValue { loc, .. }
            | IlOp::UnboxValue { loc, .. }
            | IlOp::LoadField { loc, .. }
            | IlOp::GetField { loc }
            | IlOp::SetField { loc }
            | IlOp::HostInvoke { loc, .. }
            | IlOp::Print { loc }
            | IlOp::Return { loc }
            | IlOp::Halt { loc }
            | IlOp::Bin { loc, .. }
            | IlOp::BinSlotImm { loc, .. }
            | IlOp::BinSlotSlot { loc, .. }
            | IlOp::LoadReturnSlot { loc, .. }
            | IlOp::ConstReturnImm { loc, .. }
            | IlOp::BinReturn { loc, .. }
            | IlOp::Jump { loc, .. }
            | IlOp::Entry { loc, .. }
            | IlOp::PrologueJmp { loc } => *loc,
            IlOp::Label(_) | IlOp::JoinLabel(_) => DebugLoc::unknown(),
        }
    }

    #[allow(dead_code)]
    pub fn set_loc(&mut self, loc: DebugLoc) {
        match self {
            IlOp::Byte { loc: l, .. }
            | IlOp::Load { loc: l, .. }
            | IlOp::StorePop { loc: l, .. }
            | IlOp::Const { loc: l, .. }
            | IlOp::ConstPool { loc: l, .. }
            | IlOp::String { loc: l, .. }
            | IlOp::Dup { loc: l }
            | IlOp::Pop { loc: l }
            | IlOp::Index { loc: l }
            | IlOp::IndexUnchecked { loc: l }
            | IlOp::ArrayPin { loc: l, .. }
            | IlOp::IndexPin { loc: l, .. }
            | IlOp::IndexPinUnchecked { loc: l, .. }
            | IlOp::StoreIndexPin { loc: l, .. }
            | IlOp::StoreIndexPinUnchecked { loc: l, .. }
            | IlOp::MakeTuple { loc: l, .. }
            | IlOp::MakeArray { loc: l, .. }
            | IlOp::MakeEnum { loc: l, .. }
            | IlOp::BoxValue { loc: l, .. }
            | IlOp::UnboxValue { loc: l, .. }
            | IlOp::LoadField { loc: l, .. }
            | IlOp::GetField { loc: l }
            | IlOp::SetField { loc: l }
            | IlOp::HostInvoke { loc: l, .. }
            | IlOp::Print { loc: l }
            | IlOp::Return { loc: l }
            | IlOp::Halt { loc: l }
            | IlOp::Bin { loc: l, .. }
            | IlOp::BinSlotImm { loc: l, .. }
            | IlOp::BinSlotSlot { loc: l, .. }
            | IlOp::LoadReturnSlot { loc: l, .. }
            | IlOp::ConstReturnImm { loc: l, .. }
            | IlOp::BinReturn { loc: l, .. }
            | IlOp::Jump { loc: l, .. }
            | IlOp::Entry { loc: l, .. }
            | IlOp::PrologueJmp { loc: l } => *l = loc,
            IlOp::Label(_) | IlOp::JoinLabel(_) => {}
        }
    }

    /// Encode as `Byte` when this op is a plain data/compute slot (not control).
    pub fn as_plain_byte(&self) -> Option<Byte> {
        self.as_encode_byte()
    }

    #[allow(dead_code)]
    pub fn instruction(&self) -> Option<Instruction> {
        match self {
            IlOp::Byte { byte, .. } => Some(*byte.bytecode()),
            IlOp::Load { .. } => Some(Instruction::LOAD),
            IlOp::StorePop { .. } => Some(Instruction::STORE),
            IlOp::Const { .. } | IlOp::ConstPool { .. } => Some(Instruction::CONST),
            IlOp::String { .. } => Some(Instruction::STRING),
            IlOp::Dup { .. } => Some(Instruction::DUPLICATE),
            IlOp::Pop { .. } => Some(Instruction::POP),
            IlOp::Index { .. } => Some(Instruction::Index),
            IlOp::IndexUnchecked { .. } => Some(Instruction::IndexUnchecked),
            IlOp::ArrayPin { .. } => Some(Instruction::ArrayPin),
            IlOp::IndexPin { .. } => Some(Instruction::IndexPin),
            IlOp::IndexPinUnchecked { .. } => Some(Instruction::IndexPinUnchecked),
            IlOp::StoreIndexPin { .. } => Some(Instruction::StoreIndexPin),
            IlOp::StoreIndexPinUnchecked { .. } => Some(Instruction::StoreIndexPinUnchecked),
            IlOp::MakeTuple { .. } => Some(Instruction::MakeTuple),
            IlOp::MakeArray { .. } => Some(Instruction::MakeArray),
            IlOp::MakeEnum { .. } => Some(Instruction::MakeEnum),
            IlOp::BoxValue { .. } => Some(Instruction::BoxValue),
            IlOp::UnboxValue { .. } => Some(Instruction::UnboxValue),
            IlOp::LoadField { .. } => Some(Instruction::LoadField),
            IlOp::GetField { .. } => Some(Instruction::GetField),
            IlOp::SetField { .. } => Some(Instruction::SetField),
            IlOp::HostInvoke { .. } => Some(Instruction::HostInvoke),
            IlOp::Print { .. } => Some(Instruction::PRINT),
            IlOp::Return { .. } => Some(Instruction::RETURN),
            IlOp::Halt { .. } => Some(Instruction::HALT),
            IlOp::Bin { op, .. } => Some(*op),
            IlOp::BinSlotImm { .. } => Some(Instruction::BinSlotImm),
            IlOp::BinSlotSlot { .. } => Some(Instruction::BinSlotSlot),
            IlOp::LoadReturnSlot { .. } => Some(Instruction::LoadReturnSlot),
            IlOp::ConstReturnImm { .. } => Some(Instruction::ConstReturnImm),
            IlOp::BinReturn { .. } => Some(Instruction::BinReturn),
            IlOp::Jump { kind, .. } => Some(match kind {
                IlJumpKind::Unconditional => Instruction::JMP,
                IlJumpKind::JumpIfFalse => Instruction::JMPF,
                IlJumpKind::JumpIfTrue => Instruction::JMPT,
                IlJumpKind::JumpIfMatch { .. } => Instruction::JumpIfMatch,
            }),
            IlOp::Entry { kind, .. } => Some(match kind {
                EntryKind::Call => Instruction::CALL,
                EntryKind::TailCall => Instruction::TailCall,
                EntryKind::MakeCoro => Instruction::MakeCoro,
                EntryKind::CodePtr => Instruction::CodePtr,
                EntryKind::MakePolyFn => Instruction::MakePolyFn,
            }),
            IlOp::PrologueJmp { .. } => Some(Instruction::JMP),
            IlOp::Label(_) | IlOp::JoinLabel(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_plain_byte_lifts_load_const_bin() {
        let load = IlOp::from_plain_byte(
            Byte::new(Instruction::LOAD).with_operand_u32(3),
            DebugLoc::unknown(),
        );
        assert!(matches!(load, IlOp::Load { slot: 3, .. }));
        let c = IlOp::from_plain_byte(
            Byte::new(Instruction::CONST).with_const_inline(7),
            DebugLoc::unknown(),
        );
        assert!(matches!(c, IlOp::Const { imm: 7, .. }));
        let add = IlOp::from_plain_byte(Byte::new(Instruction::ADD), DebugLoc::unknown());
        assert!(matches!(
            add,
            IlOp::Bin {
                op: Instruction::ADD,
                ..
            }
        ));
    }

    #[test]
    fn from_plain_byte_lifts_index_make_tuple_array_enum() {
        assert!(matches!(
            IlOp::from_plain_byte(Byte::new(Instruction::Index), DebugLoc::unknown()),
            IlOp::Index { .. }
        ));
        assert!(matches!(
            IlOp::from_plain_byte(
                Byte::new(Instruction::MakeTuple).with_operand_u32(2),
                DebugLoc::unknown(),
            ),
            IlOp::MakeTuple { arity: 2, .. }
        ));
        assert!(matches!(
            IlOp::from_plain_byte(
                Byte::new(Instruction::MakeArray).with_operand_u32(3),
                DebugLoc::unknown(),
            ),
            IlOp::MakeArray { arity: 3, .. }
        ));
        assert!(matches!(
            IlOp::from_plain_byte(
                Byte::new(Instruction::MakeEnum).with_operands_u16([9, 1]),
                DebugLoc::unknown(),
            ),
            IlOp::MakeEnum {
                tag: 9,
                arity: 1,
                ..
            }
        ));
    }

    #[test]
    fn from_plain_byte_lifts_box_unbox_load_field() {
        assert!(matches!(
            IlOp::from_plain_byte(
                Byte::new(Instruction::BoxValue).with_operand_u32(3),
                DebugLoc::unknown(),
            ),
            IlOp::BoxValue { tag: 3, .. }
        ));
        assert!(matches!(
            IlOp::from_plain_byte(
                Byte::new(Instruction::UnboxValue).with_operand_u32(4),
                DebugLoc::unknown(),
            ),
            IlOp::UnboxValue { tag: 4, .. }
        ));
        assert!(matches!(
            IlOp::from_plain_byte(
                Byte::new(Instruction::LoadField).with_operand_u32(2),
                DebugLoc::unknown(),
            ),
            IlOp::LoadField { index: 2, .. }
        ));
    }

    #[test]
    fn from_plain_byte_lifts_store_dup_pop_return_halt() {
        assert!(matches!(
            IlOp::from_plain_byte(
                Byte::new(Instruction::STORE).with_load_store_slot(5),
                DebugLoc::unknown(),
            ),
            IlOp::StorePop { slot: 5, .. }
        ));
        // Deprecated StorePop discriminant with legacy wide operand still lifts.
        assert!(matches!(
            IlOp::from_plain_byte(
                Byte::new(Instruction::StorePop).with_operand_u32(5),
                DebugLoc::unknown(),
            ),
            IlOp::StorePop { slot: 5, .. }
        ));
        assert!(matches!(
            IlOp::from_plain_byte(Byte::new(Instruction::DUPLICATE), DebugLoc::unknown()),
            IlOp::Dup { .. }
        ));
        assert!(matches!(
            IlOp::from_plain_byte(Byte::new(Instruction::POP), DebugLoc::unknown()),
            IlOp::Pop { .. }
        ));
        assert!(matches!(
            IlOp::from_plain_byte(Byte::new(Instruction::RETURN), DebugLoc::unknown()),
            IlOp::Return { .. }
        ));
        assert!(matches!(
            IlOp::from_plain_byte(Byte::new(Instruction::HALT), DebugLoc::unknown()),
            IlOp::Halt { .. }
        ));
    }

    /// Multi-slot packed LOAD/STORE must stay residual `Byte` (no single-slot typed lift).
    #[test]
    fn from_plain_byte_keeps_packed_multi_slot_load_store_as_byte() {
        let load3 = Byte::new(Instruction::LOAD).with_load_store_packed(3, 0, 1, 2);
        assert!(matches!(
            IlOp::from_plain_byte(load3, DebugLoc::unknown()),
            IlOp::Byte { .. }
        ));
        let store2 = Byte::new(Instruction::STORE).with_load_store_packed(2, 4, 5, 0);
        assert!(matches!(
            IlOp::from_plain_byte(store2, DebugLoc::unknown()),
            IlOp::Byte { .. }
        ));
        // New fuse opcodes also remain residual until typed lift is added.
        let jmpf = Byte::new(Instruction::BinSlotSlotJmpf).with_bin_slot_slot_jmpf(
            Instruction::LE as u8,
            0,
            1,
        );
        assert!(matches!(
            IlOp::from_plain_byte(jmpf, DebugLoc::unknown()),
            IlOp::Byte { .. }
        ));
        let imm_store = Byte::new(Instruction::BinSlotImmStore).with_bin_slot_imm_store(
            Instruction::ADD as u8,
            0,
            2,
        );
        assert!(matches!(
            IlOp::from_plain_byte(imm_store, DebugLoc::unknown()),
            IlOp::Byte { .. }
        ));
    }

    #[test]
    fn from_plain_byte_lifts_bin_slot_and_fused_returns() {
        let imm =
            Byte::new(Instruction::BinSlotImm).with_bin_slot_imm(Instruction::ADD as u8, 2, -3);
        assert!(matches!(
            IlOp::from_plain_byte(imm, DebugLoc::unknown()),
            IlOp::BinSlotImm {
                op,
                slot: 2,
                imm: -3,
                ..
            } if op == Instruction::ADD as u8
        ));
        let slot =
            Byte::new(Instruction::BinSlotSlot).with_bin_slot_slot(Instruction::SUB as u8, 0, 1);
        assert!(matches!(
            IlOp::from_plain_byte(slot, DebugLoc::unknown()),
            IlOp::BinSlotSlot {
                op,
                a: 0,
                b: 1,
                ..
            } if op == Instruction::SUB as u8
        ));
        assert!(matches!(
            IlOp::from_plain_byte(
                Byte::new(Instruction::LoadReturnSlot).with_operand_u32(4),
                DebugLoc::unknown(),
            ),
            IlOp::LoadReturnSlot { slot: 4, .. }
        ));
        assert!(matches!(
            IlOp::from_plain_byte(
                Byte::new(Instruction::ConstReturnImm).with_operand_u32(9),
                DebugLoc::unknown(),
            ),
            IlOp::ConstReturnImm { imm: 9, .. }
        ));
        assert!(matches!(
            IlOp::from_plain_byte(
                Byte::new(Instruction::BinReturn).with_bin_return(Instruction::MUL as u8),
                DebugLoc::unknown(),
            ),
            IlOp::BinReturn {
                op: Instruction::MUL,
                ..
            }
        ));
    }

    #[test]
    fn long_tail_control_stays_unencoded() {
        let jmp = IlOp::jump(IlJumpKind::Unconditional, Label(0), DebugLoc::unknown());
        assert!(jmp.as_encode_byte().is_none());
        assert!(jmp.is_control());
        let entry = IlOp::Entry {
            kind: EntryKind::Call,
            arity: 1,
            target: Label(1),
            loc: DebugLoc::unknown(),
        };
        assert!(entry.as_encode_byte().is_none());
        assert!(entry.is_control());
        assert!(IlOp::Label(Label(0)).as_encode_byte().is_none());
        assert!(!IlOp::Label(Label(0)).emits_code());
    }

    #[test]
    fn from_plain_byte_lifts_host_print_get_set_field() {
        assert!(matches!(
            IlOp::from_plain_byte(
                Byte::new(Instruction::HostInvoke).with_operand_u32(2),
                DebugLoc::unknown(),
            ),
            IlOp::HostInvoke { arity: 2, .. }
        ));
        assert!(matches!(
            IlOp::from_plain_byte(Byte::new(Instruction::PRINT), DebugLoc::unknown()),
            IlOp::Print { .. }
        ));
        assert!(matches!(
            IlOp::from_plain_byte(
                Byte::new(Instruction::STRING).with_operand_u32(9),
                DebugLoc::unknown(),
            ),
            IlOp::String { idx: 9, .. }
        ));
        assert!(matches!(
            IlOp::from_plain_byte(Byte::new(Instruction::GetField), DebugLoc::unknown()),
            IlOp::GetField { .. }
        ));
        assert!(matches!(
            IlOp::from_plain_byte(Byte::new(Instruction::SetField), DebugLoc::unknown()),
            IlOp::SetField { .. }
        ));
    }

    #[test]
    fn is_plain_return_excludes_fused_returns() {
        assert!(
            IlOp::Return {
                loc: DebugLoc::unknown()
            }
            .is_plain_return()
        );
        assert!(IlOp::byte(Byte::new(Instruction::RETURN)).is_plain_return());
        assert!(
            !IlOp::ConstReturnImm {
                imm: 0,
                loc: DebugLoc::unknown()
            }
            .is_plain_return()
        );
        assert!(
            !IlOp::LoadReturnSlot {
                slot: 0,
                loc: DebugLoc::unknown()
            }
            .is_plain_return()
        );
        assert!(
            !IlOp::BinReturn {
                op: Instruction::ADD,
                loc: DebugLoc::unknown()
            }
            .is_plain_return()
        );
        assert!(!IlOp::byte(Byte::new(Instruction::ReturnPair)).is_plain_return());
    }

    #[test]
    fn as_encode_byte_round_trips_hot_set() {
        let ops = [
            IlOp::Load {
                slot: 2,
                loc: DebugLoc::unknown(),
            },
            IlOp::StorePop {
                slot: 3,
                loc: DebugLoc::unknown(),
            },
            IlOp::Const {
                imm: 42,
                loc: DebugLoc::unknown(),
            },
            IlOp::String {
                idx: 5,
                loc: DebugLoc::unknown(),
            },
            IlOp::Dup {
                loc: DebugLoc::unknown(),
            },
            IlOp::Pop {
                loc: DebugLoc::unknown(),
            },
            IlOp::Return {
                loc: DebugLoc::unknown(),
            },
            IlOp::Halt {
                loc: DebugLoc::unknown(),
            },
            IlOp::Bin {
                op: Instruction::SUB,
                loc: DebugLoc::unknown(),
            },
            IlOp::BinSlotImm {
                op: Instruction::ADD as u8,
                slot: 1,
                imm: 4,
                loc: DebugLoc::unknown(),
            },
            IlOp::BinSlotSlot {
                op: Instruction::ADD as u8,
                a: 0,
                b: 1,
                loc: DebugLoc::unknown(),
            },
            IlOp::LoadReturnSlot {
                slot: 2,
                loc: DebugLoc::unknown(),
            },
            IlOp::ConstReturnImm {
                imm: 5,
                loc: DebugLoc::unknown(),
            },
            IlOp::BinReturn {
                op: Instruction::MUL,
                loc: DebugLoc::unknown(),
            },
            IlOp::Index {
                loc: DebugLoc::unknown(),
            },
            IlOp::MakeTuple {
                arity: 2,
                loc: DebugLoc::unknown(),
            },
            IlOp::MakeArray {
                arity: 3,
                loc: DebugLoc::unknown(),
            },
            IlOp::MakeEnum {
                tag: 9,
                arity: 1,
                loc: DebugLoc::unknown(),
            },
            IlOp::BoxValue {
                tag: 3,
                loc: DebugLoc::unknown(),
            },
            IlOp::UnboxValue {
                tag: 4,
                loc: DebugLoc::unknown(),
            },
            IlOp::LoadField {
                index: 2,
                loc: DebugLoc::unknown(),
            },
        ];
        for op in ops {
            let b = op.as_encode_byte().expect("encode");
            let again = IlOp::from_plain_byte(b, DebugLoc::unknown());
            assert_eq!(again.as_encode_byte(), Some(b));
            assert!(again == op);
        }
    }

    #[test]
    fn pool_const_lifts_to_const_pool() {
        let pool = Byte::new(Instruction::CONST).with_const_pool(3);
        let op = IlOp::from_plain_byte(pool, DebugLoc::unknown());
        assert!(matches!(op, IlOp::ConstPool { idx: 3, .. }));
        assert_eq!(op.as_encode_byte(), Some(pool));
    }

    #[test]
    fn negative_inline_const_lifts_as_const_pool_encoding() {
        // Inline CONST uses bit 31 as POOL_FLAG; negative i32 values set it.
        // Absorb as ConstPool so encoding round-trips (same bit pattern).
        let neg = Byte::new(Instruction::CONST).with_const_inline(-1);
        assert_ne!(neg.operand_u32() & Byte::POOL_FLAG, 0);
        let op = IlOp::from_plain_byte(neg, DebugLoc::unknown());
        assert!(matches!(op, IlOp::ConstPool { .. }));
        assert_eq!(op.as_encode_byte(), Some(neg));
    }

    #[test]
    fn as_encode_byte_round_trips_residual_typed() {
        let ops = [
            IlOp::ConstPool {
                idx: 1,
                loc: DebugLoc::unknown(),
            },
            IlOp::GetField {
                loc: DebugLoc::unknown(),
            },
            IlOp::SetField {
                loc: DebugLoc::unknown(),
            },
            IlOp::HostInvoke {
                arity: 2,
                loc: DebugLoc::unknown(),
            },
            IlOp::Print {
                loc: DebugLoc::unknown(),
            },
        ];
        for op in ops {
            let b = op.as_encode_byte().expect("encode");
            let again = IlOp::from_plain_byte(b, DebugLoc::unknown());
            assert_eq!(again.as_encode_byte(), Some(b));
            assert!(again == op);
        }
    }
}
