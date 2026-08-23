//! VM instruction set and encoded bytecode words (`Byte`).
//!
//! Append new `Instruction` variants only — `#[repr(u8)]` discriminants
//! must stay stable for archived bytecode.

use rkyv::{Archive, Deserialize, Serialize};

use crate::Value;

#[repr(u8)]
#[derive(Default, Copy, Clone, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[cfg_attr(debug_assertions, derive(Debug))]
#[rkyv(compare(PartialEq), derive(Clone), derive(Copy))]
pub enum Instruction {
    // Special
    #[default]
    HALT,
    NOOP,
    DUPLICATE,
    POP,
    CONST,
    // STORE/LOAD: [31:24]=n (1..=3), [23:16]=s2, [15:8]=s1, [7:0]=s0;
    // n==0 → wide single slot in [23:0]. See `with_load_store_*`.
    STORE,
    LOAD,
    CALL,
    RETURN,
    JMP,
    JMPT,
    JMPF,
    /// Index into [`crate::archive::ArchivedProgram::strings`] (v35+).
    /// Pre-v35 archives used `STRING` + trailing `DATA` char words.
    STRING,
    /// Tombstone: pre-v35 char payload after `STRING`. Never emitted by the compiler.
    DATA,
    INC,
    DEC,

    // Arithmetic
    ADD,
    SUB,
    MUL,
    DIV,
    MOD,
    ADDF,
    SUBF,
    MULF,
    DIVF,
    MODF,
    NOT,
    NEG,
    AND,
    OR,
    SHL,
    SHR,
    XOR,
    EQ,
    NEQ,
    LE,
    LEQ,
    LEF,
    LEQF,
    GT,
    GEQ,
    GTF,
    GEQF,

    // Built-ins
    PRINT,
    FORMAT,
    STRINGIFY,
    NATIVE,
    INIT,
    SET,

    // Sum types — append-only beyond this point.
    //
    // MakeEnum:     [31:16] tag, [15:0] arity
    // JumpIfMatch:  [31:16] tag, [15:0] pool index → 32-bit target
    // Unpack:       [31:0] arity
    // LoadField:    [15:0] field_index
    MakeEnum,
    JumpIfMatch,
    Unpack,
    LoadField,

    // StorePop: deprecated discriminant alias of STORE (same VM handler).
    // Compiler emits STORE only. INC/DEC: [31:3] slot, [2] prefix, [1] float.
    StorePop,

    // UnpackAt: [31:16] arity, [15:0] slot offset — unpack enum at slot in place.
    UnpackAt,

    // FFI — stack bottom→top unless noted.
    FfiLoad,
    // FfiInvoke: [15:0] arity (informational);
    //            [16] = has_arg_type_tags (C varargs call);
    // stack: lib, fn_id, args_tuple [, tags_tuple if bit 16]
    FfiInvoke,
    // DeclareFFI: [15:0] fixed arity (tag tuple length);
    //             [16] = variadic flag (C `...`);
    // stack: lib, name, args_tuple (fixed tags only), ret_tag
    DeclareFFI,

    // Aggregates — MakeTuple/MakeArray/MakeDict: [15:0] arity; Index: no operand
    MakeTuple,
    MakeArray,
    Index,

    // Records — MakeDict: [15:0] field count; GetField/SetField: no operand
    MakeDict,
    GetField,
    SetField,

    // HostInvoke: [15:0] tuple arity; stack: fn_id, args_tuple
    HostInvoke,

    // Fused superinstructions — underlying op in [31:24] where applicable.
    //
    // LoadReturnSlot: [31:0] slot
    // ConstReturnImm: [31:0] inline i32
    // BinSlotImm:     [31:24] op, [23:16] slot, [15:0] i16 imm
    // CmpJmpf:        [31:24] op, [15:0] false-branch target
    // BinReturn:      [31:24] op
    // BinSlotSlot:    [31:24] op, [23:16] slot a, [15:8] slot b
    LoadReturnSlot,
    ConstReturnImm,
    BinSlotImm,
    CmpJmpf,
    BinReturn,
    BinSlotSlot,
    /// `LOAD slot; CONST imm; <cmp>; JMPF t` — pool entry packs imm (low 32) + target (high 32).
    BinSlotImmJmpf,
    /// `LogNot; JMPF t` — branch when logical-not result is false.
    LogNotJmpf,

    // Coroutines — append-only.
    //
    // MakeCoro:  [31:24] arity, [23:0] entry target (same layout as CALL)
    // ResumeCoro: operands[0] & 1 = has_send; stack [..., send, handle] (TOS = handle)
    // YieldCoro:  no operand; pops yield value
    // YieldFromCoro: no operand; pops sub-coroutine handle
    MakeCoro,
    ResumeCoro,
    YieldCoro,
    YieldFromCoro,

    // Power — append-only.
    Pow,
    PowF,

    // Bitwise AND/OR distinct from logical AND/OR.
    BITAND,
    BITOR,

    /// Pop `value`, `index`, `target`; store into array element; push `value`.
    StoreIndex,

    /// Logical NOT: bool or int (zero vs non-zero) → bool.
    LogNot,

    /// Coroutine done-check: pop handle, push `true` if `CoroState::Done`.
    DoneCoro,

    /// ArrayPush: stack `array, value` (TOS = value) -> same array after in-place append.
    /// ArrayLen: stack `array|string|tuple|dict` -> int length.
    ArrayPush,
    ArrayLen,

    // Generics runtime — append-only.
    /// CallIndirect: stack `[value_args..., app_dicts..., target]` (TOS = target).
    ///
    /// Operand packing:
    /// - `[15:0]`  = value arity (non-dictionary arguments)
    /// - `[31:16]` = application dictionary arity (0 for plain code-offset calls)
    ///
    /// When the target is an `ObjPolyFn` with captured evidence, the VM merges
    /// `captured_dicts` with the application dictionaries (preferring captures)
    /// before setting up the callee frame. Plain integer/`CodePtr` targets ignore
    /// captures and treat `[15:0]` as the full argument count when `[31:16] == 0`
    /// for backward compatibility (`arity = value_arity + app_dict_arity`).
    CallIndirect,
    /// BoxValue: [15:0] ValueTag as u16; pop raw value → push Object::Boxed pointer
    BoxValue,
    /// UnboxValue: [15:0] expected ValueTag; pop Boxed → push payload or leave default on mismatch
    UnboxValue,
    /// MakePolyFn: [31:0] entry offset; push Object::PolyFn
    MakePolyFn,
    /// DynAdd / DynSub / DynMul / DynDiv / DynMod — pop b, a (boxed or immediate); push result
    DynAdd,
    DynSub,
    DynMul,
    DynDiv,
    DynMod,
    /// DynCmp: [7:0] kind (0=Le,1=Leq,2=Gt,3=Geq); pop b,a; push bool
    DynCmp,
    /// DynEq / DynNe — pop b,a; push bool
    DynEq,
    DynNe,
    /// DynPrint — pop one boxed/immediate; write Display-ish to output (int/float/bool/string)
    DynPrint,

    /// CodePtr: [31:0] absolute bytecode entry offset.
    ///
    /// Self-identifying code pointer used by dictionary method slots and
    /// direct `CallIndirect` targets. Distinct from `CONST` so peephole
    /// fusion can relocate these offsets without mistaking them for data.
    CodePtr,
    /// MakePolyFnCapture: stack `[captured dictionaries..., CodePtr entry]` →
    /// `ObjPolyFn`. `operands[7:0]` is the number of dictionary slots in
    /// declaration order. A null (`0`) slot is stored as unresolved (`None`)
    /// and filled at `CallIndirect` from application evidence.
    MakePolyFnCapture,

    /// Panic: pop string message, write `panic: <msg>` to output, abort the VM.
    /// Appended after `MakePolyFnCapture` (Phase prelude::test).
    Panic,

    /// DictEntries: pop dict (`ObjInstance`) → push `ObjArray` of
    /// `ObjTuple(2)` pairs `(key_string, value)` in table iteration order.
    /// Used by homogeneous-record `IntoIterator` / `for x in dict`.
    DictEntries,

    /// MakeFn — allocate a first-class monomorphic function / partial / lambda.
    ///
    /// Stack (bottom → TOS):
    /// `[captures..., filled_param_values..., filled_mask, entry]`
    /// Operand packing:
    /// - `[7:0]`   = capture count
    /// - `[15:8]`  = filled param count
    /// - `[23:16]` = arity (fixed N, or rest nfixed)
    /// - `[24]`    = is_rest flag
    /// `filled_mask` is an int on the stack (bit i ⇒ fixed param i is bound).
    /// `entry` is a code offset (CodePtr / int).
    MakeFn,

    /// LoadStatic: operands[31:0] = static slot index → push statics[slot].
    LoadStatic,
    /// StoreStatic: pop value, write statics[operands[31:0]].
    StoreStatic,

    /// TailCall: same packing as CALL — reuse frame (self tail recursion).
    TailCall,

    // Primitive casts — append-only (`expr as int` / `Into` thunks).
    CastIntToFloat,
    CastFloatToInt,
    CastIntToByte,
    CastByteToInt,
    CastIntToBool,
    CastBoolToInt,

    /// `BinSlotSlot; JMPF t` — pool packs slot b (low 8) + target (high 32).
    /// Operands: [31:24] op, [23:16] a, [15:0] pool index (mirrors BinSlotImmJmpf).
    BinSlotSlotJmpf,

    /// `BinSlotImm; STORE dest` — pool packs imm (low 32) + dest (high 32).
    /// Operands: [31:24] op, [23:16] src, [15:0] pool index.
    BinSlotImmStore,
    /// `BinSlotSlot; STORE dest` — no pool.
    /// Operands: [31:24] op, [23:16] a, [15:8] b, [7:0] dest.
    BinSlotSlotStore,

    /// Seek: set shared operand/local cursor to `sp + operands[31:0]`.
    ///
    /// Used before `JumpIfMatch` so payloads land at the compile-time
    /// `payload_base` even when prior `STORE`s raised the high-water mark
    /// (shared stack/locals; see match-in-loop bindings).
    Seek,

    /// Convert a pointer-niche `Option<T>` (`0` / heap payload) to a boxed
    /// `Option<T>` enum at a representation boundary.
    OptionNicheToHeap,
    /// Convert a boxed `Option<T>` enum to its pointer-niche representation.
    HeapOptionToNiche,

    /// Test the tag at the top of a unary stack pair `[payload, tag]`.
    /// A matching tag is consumed and branches to the packed target.
    PairJumpIfTag,
    /// Box a unary stack pair `[payload, tag]` as a heap enum.
    PairToHeap,
    /// Unbox a unary heap enum into `[payload, tag]`.
    HeapToPair,
    /// Return a unary stack pair `[payload, tag]` to the caller.
    ReturnPair,
    /// Invoke a known host native that returns pointer-niche `Option<T>`.
    HostInvokeNiche,
    /// Evaluate two or three source-ordered float binary stages and store.
    ///
    /// Opcode operand: `[31:16] dest_slot`, `[15:0] descriptor pool index`.
    ///
    /// Legacy descriptor (bit 63 clear): two stages, slot operands only —
    /// `[7:0] op0`, `[15:8] lhs0`, `[23:16] rhs0`, `[31:24] op1`, `[39:32] rhs1`.
    /// Acc starts as `op0(slot[lhs0], slot[rhs0])`, then `op1(acc, slot[rhs1])`.
    ///
    /// Extended (bit 63 set): optional third stage and const-pool operands —
    /// same low fields, plus `[47:40] op2`, `[55:48] rhs2`, flags in `[62:56]`:
    /// bit56 rhs0_const, bit57 rhs1_const, bit58 rhs2_const, bit59 lhs0_const,
    /// bit60 stage1_other_on_left, bit61 stage2_other_on_left, bit62 has_stage2.
    /// When `other_on_left`, that stage is `op(other, acc)` (stack const-under).
    /// Stage0 may be `LOAD;LOAD`, `BinSlotSlot`, or `LOAD`/`CONST` mix (const flags).
    FloatChainStore,

    /// `BinSlotSlot <float-arith>; CONST pool; CmpJmpf <float-cmp>` — one dispatch.
    ///
    /// Operands: `[31:24] bin_op`, `[23:16] a`, `[15:0] descriptor pool index`.
    /// Descriptor: `[7:0] b`, `[15:8] cmp_op`, `[31:16] float_pool_idx`,
    /// `[63:32] false-branch target PC`. Evaluates `bin_op(slot[a], slot[b])`
    /// then compares with the pool float (source order; no FMA/reassoc).
    BinSlotSlotConstJmpf,

    /// Float unary negate (IEEE sign-bit flip). Replaces `CONST -1; MULF`.
    NEGF,

    /// Allocate a class instance stamped with a compile-time type id.
    ///
    /// Operand is `type_id` (`0` is unused — prefer [`Self::INIT`] for untyped
    /// bags such as dicts). Existing [`Self::INIT`] stays for old archives.
    InitTyped,

    /// Jump-if-true twins of the `*Jmpf` family (same operand packing).
    /// `invert_branch_over_jump` emits these so fused `*Jmpf; JMP` can collapse.
    CmpJmpt,
    BinSlotImmJmpt,
    LogNotJmpt,
    BinSlotSlotJmpt,
    BinSlotSlotConstJmpt,

    /// Bounds-proofed [`Self::Index`]: compiler guarantees `0 <= index < len` for
    /// the addressed array/tuple. UB in release on violation.
    IndexUnchecked,
    /// Bounds-proofed [`Self::StoreIndex`]: same guarantee as [`Self::IndexUnchecked`].
    StoreIndexUnchecked,
}

impl From<u8> for Instruction {
    fn from(value: u8) -> Self {
        unsafe { std::mem::transmute(value) }
    }
}

impl From<Instruction> for u8 {
    fn from(val: Instruction) -> Self {
        val as u8
    }
}

impl Instruction {
    /// Stable mnemonic for disassembly / DX tooling (always available).
    pub fn mnemonic(self) -> &'static str {
        match self {
            Self::HALT => "HALT",
            Self::NOOP => "NOOP",
            Self::DUPLICATE => "DUPLICATE",
            Self::POP => "POP",
            Self::CONST => "CONST",
            Self::STORE => "STORE",
            Self::LOAD => "LOAD",
            Self::CALL => "CALL",
            Self::RETURN => "RETURN",
            Self::JMP => "JMP",
            Self::JMPT => "JMPT",
            Self::JMPF => "JMPF",
            Self::STRING => "STRING",
            Self::DATA => "DATA",
            Self::INC => "INC",
            Self::DEC => "DEC",
            Self::ADD => "ADD",
            Self::SUB => "SUB",
            Self::MUL => "MUL",
            Self::DIV => "DIV",
            Self::MOD => "MOD",
            Self::ADDF => "ADDF",
            Self::SUBF => "SUBF",
            Self::MULF => "MULF",
            Self::DIVF => "DIVF",
            Self::MODF => "MODF",
            Self::NOT => "NOT",
            Self::NEG => "NEG",
            Self::AND => "AND",
            Self::OR => "OR",
            Self::SHL => "SHL",
            Self::SHR => "SHR",
            Self::XOR => "XOR",
            Self::EQ => "EQ",
            Self::NEQ => "NEQ",
            Self::LE => "LE",
            Self::LEQ => "LEQ",
            Self::LEF => "LEF",
            Self::LEQF => "LEQF",
            Self::GT => "GT",
            Self::GEQ => "GEQ",
            Self::GTF => "GTF",
            Self::GEQF => "GEQF",
            Self::PRINT => "PRINT",
            Self::FORMAT => "FORMAT",
            Self::STRINGIFY => "STRINGIFY",
            Self::NATIVE => "NATIVE",
            Self::INIT => "INIT",
            Self::SET => "SET",
            Self::MakeEnum => "MakeEnum",
            Self::JumpIfMatch => "JumpIfMatch",
            Self::Unpack => "Unpack",
            Self::LoadField => "LoadField",
            Self::StorePop => "StorePop",
            Self::UnpackAt => "UnpackAt",
            Self::FfiLoad => "FfiLoad",
            Self::FfiInvoke => "FfiInvoke",
            Self::DeclareFFI => "DeclareFFI",
            Self::MakeTuple => "MakeTuple",
            Self::MakeArray => "MakeArray",
            Self::Index => "Index",
            Self::MakeDict => "MakeDict",
            Self::GetField => "GetField",
            Self::SetField => "SetField",
            Self::HostInvoke => "HostInvoke",
            Self::LoadReturnSlot => "LoadReturnSlot",
            Self::ConstReturnImm => "ConstReturnImm",
            Self::BinSlotImm => "BinSlotImm",
            Self::CmpJmpf => "CmpJmpf",
            Self::BinReturn => "BinReturn",
            Self::BinSlotSlot => "BinSlotSlot",
            Self::BinSlotImmJmpf => "BinSlotImmJmpf",
            Self::LogNotJmpf => "LogNotJmpf",
            Self::MakeCoro => "MakeCoro",
            Self::ResumeCoro => "ResumeCoro",
            Self::YieldCoro => "YieldCoro",
            Self::YieldFromCoro => "YieldFromCoro",
            Self::Pow => "Pow",
            Self::PowF => "PowF",
            Self::BITAND => "BITAND",
            Self::BITOR => "BITOR",
            Self::StoreIndex => "StoreIndex",
            Self::LogNot => "LogNot",
            Self::DoneCoro => "DoneCoro",
            Self::ArrayPush => "ArrayPush",
            Self::ArrayLen => "ArrayLen",
            Self::CallIndirect => "CallIndirect",
            Self::BoxValue => "BoxValue",
            Self::UnboxValue => "UnboxValue",
            Self::MakePolyFn => "MakePolyFn",
            Self::DynAdd => "DynAdd",
            Self::DynSub => "DynSub",
            Self::DynMul => "DynMul",
            Self::DynDiv => "DynDiv",
            Self::DynMod => "DynMod",
            Self::DynCmp => "DynCmp",
            Self::DynEq => "DynEq",
            Self::DynNe => "DynNe",
            Self::DynPrint => "DynPrint",
            Self::CodePtr => "CodePtr",
            Self::MakePolyFnCapture => "MakePolyFnCapture",
            Self::Panic => "Panic",
            Self::DictEntries => "DictEntries",
            Self::MakeFn => "MakeFn",
            Self::LoadStatic => "LoadStatic",
            Self::StoreStatic => "StoreStatic",
            Self::TailCall => "TailCall",
            Self::CastIntToFloat => "CastIntToFloat",
            Self::CastFloatToInt => "CastFloatToInt",
            Self::CastIntToByte => "CastIntToByte",
            Self::CastByteToInt => "CastByteToInt",
            Self::CastIntToBool => "CastIntToBool",
            Self::CastBoolToInt => "CastBoolToInt",
            Self::BinSlotSlotJmpf => "BinSlotSlotJmpf",
            Self::BinSlotImmStore => "BinSlotImmStore",
            Self::BinSlotSlotStore => "BinSlotSlotStore",
            Self::Seek => "Seek",
            Self::OptionNicheToHeap => "OptionNicheToHeap",
            Self::HeapOptionToNiche => "HeapOptionToNiche",
            Self::PairJumpIfTag => "PairJumpIfTag",
            Self::PairToHeap => "PairToHeap",
            Self::HeapToPair => "HeapToPair",
            Self::ReturnPair => "ReturnPair",
            Self::HostInvokeNiche => "HostInvokeNiche",
            Self::FloatChainStore => "FloatChainStore",
            Self::BinSlotSlotConstJmpf => "BinSlotSlotConstJmpf",
            Self::NEGF => "NEGF",
            Self::InitTyped => "InitTyped",
            Self::CmpJmpt => "CmpJmpt",
            Self::BinSlotImmJmpt => "BinSlotImmJmpt",
            Self::LogNotJmpt => "LogNotJmpt",
            Self::BinSlotSlotJmpt => "BinSlotSlotJmpt",
            Self::BinSlotSlotConstJmpt => "BinSlotSlotConstJmpt",
            Self::IndexUnchecked => "IndexUnchecked",
            Self::StoreIndexUnchecked => "StoreIndexUnchecked",
        }
    }
}

impl From<u8> for ArchivedInstruction {
    fn from(value: u8) -> Self {
        unsafe { std::mem::transmute(value) }
    }
}

#[repr(C)]
#[derive(Default, Clone, Copy, PartialEq, Eq, Archive, Serialize, Deserialize)]
#[rkyv(compare(PartialEq))]
pub struct Byte {
    bytecode: Instruction,
    _pad: [u8; 3],
    operands: u32,
}

impl Byte {
    /// High bit on a `CONST` operand marks a constant-pool index.
    pub const POOL_FLAG: u32 = 1 << 31;

    pub fn new(bytecode: Instruction) -> Self {
        Self {
            bytecode,
            _pad: [0; 3],
            operands: Default::default(),
        }
    }

    pub fn with_operand_u32(mut self, operand: u32) -> Self {
        self.operands = operand;
        self
    }

    /// CALL: [31:24] arity, [23:0] target (24 bits).
    pub fn with_call_packed(mut self, arity: u32, target: u32) -> Self {
        debug_assert!(target <= 0xFFFFFF, "CALL target exceeds 24-bit encoding");
        self.operands = (arity << 24) | (target & 0xFFFFFF);
        self
    }

    pub fn call_parts(&self) -> (usize, usize) {
        (
            (self.operands >> 24) as usize,
            (self.operands & 0xFFFFFF) as usize,
        )
    }

    pub fn with_value_u32(mut self, v: u32) -> Self {
        if matches!(self.bytecode, Instruction::CALL) {
            let arity = self.operands;
            return self.with_call_packed(arity, v);
        }
        self.operands = (self.operands & 0xFFFF_0000) | (v & 0xFFFF);
        self
    }

    pub fn value_u32(&self) -> u32 {
        if matches!(self.bytecode, Instruction::CALL) {
            self.operands & 0xFFFFFF
        } else {
            self.operands & 0xFFFF
        }
    }

    pub fn with_operands_u16(mut self, operands: [u16; 2]) -> Self {
        let mut operand: u32 = 0;
        operand ^= operands[0] as u32;
        operand <<= 16;
        operand ^= operands[1] as u32;
        self.operands = operand;
        self
    }

    pub fn with_const_inline(mut self, value: i32) -> Self {
        debug_assert!(self.bytecode as u8 == Instruction::CONST as u8);
        self.operands = value as u32;
        self
    }

    pub fn with_const_pool(mut self, pool_index: u32) -> Self {
        debug_assert!(self.bytecode as u8 == Instruction::CONST as u8);
        self.operands = Self::POOL_FLAG | pool_index;
        self
    }

    pub fn new_with_value(bytecode: Instruction, value: u64) -> Self {
        let v = value as i64;
        if v >= i32::MIN as i64 && v <= i32::MAX as i64 {
            Self::new(bytecode).with_const_inline(v as i32)
        } else {
            Self::new(bytecode).with_const_pool(0)
        }
    }

    pub fn bytecode(&self) -> &Instruction {
        &self.bytecode
    }

    pub fn operand_u32(&self) -> u32 {
        self.operands
    }

    pub fn operand_u16(&self, index: usize) -> u16 {
        match index {
            0 => (self.operands >> 16) as u16,
            1 => ((self.operands << 16) >> 16) as u16,
            _ => unreachable!("Unable to use larger index when using u32 operands"),
        }
    }

    pub fn constant(&self, pool: &[u64]) -> u64 {
        if self.operands & Self::POOL_FLAG != 0 {
            let idx = (self.operands & !Self::POOL_FLAG) as usize;
            crate::promise!(idx < pool.len());
            // SAFETY: promise! guarantees idx < pool.len().
            unsafe { *pool.get_unchecked(idx) }
        } else {
            self.operands as i32 as i64 as u64
        }
    }

    /// JumpIfMatch target from pool (index in lower 16 bits of `operands`).
    pub fn jump_if_match_target(&self, pool: &[u64]) -> usize {
        let idx = (self.operands & 0xFFFF) as usize;
        crate::promise!(idx < pool.len());
        // SAFETY: promise! guarantees idx < pool.len().
        unsafe { *pool.get_unchecked(idx) as usize }
    }

    pub fn with_bin_slot_imm(mut self, op: u8, slot: u8, imm: i16) -> Self {
        self.operands = ((op as u32) << 24) | ((slot as u32) << 16) | (imm as u16 as u32);
        self
    }

    pub fn bin_slot_imm_parts(&self) -> (u8, usize, i64) {
        let o = self.operands;
        (
            (o >> 24) as u8,
            ((o >> 16) & 0xFF) as usize,
            (o & 0xFFFF) as u16 as i16 as i64,
        )
    }

    /// CmpJmpf direct target: [31:24] op, [15:0] PC (bit 16 clear).
    pub fn with_cmp_jmpf(mut self, op: u8, target: u16) -> Self {
        self.operands = ((op as u32) << 24) | (target as u32);
        self
    }

    /// CmpJmpf pool target: [31:24] op, bit 16 set, [15:0] pool index → absolute PC.
    pub fn with_cmp_jmpf_pool(mut self, op: u8, pool_idx: u16) -> Self {
        self.operands = ((op as u32) << 24) | (1u32 << 16) | (pool_idx as u32);
        self
    }

    pub fn cmp_jmpf_parts(&self) -> (u8, usize) {
        (
            (self.operands >> 24) as u8,
            (self.operands & 0xFFFF) as usize,
        )
    }

    pub fn cmp_jmpf_is_pool(&self) -> bool {
        (self.operands & (1u32 << 16)) != 0
    }

    pub fn with_bin_return(mut self, op: u8) -> Self {
        self.operands = (op as u32) << 24;
        self
    }

    pub fn bin_return_op(&self) -> u8 {
        (self.operands >> 24) as u8
    }

    pub fn with_bin_slot_slot(mut self, op: u8, a: u8, b: u8) -> Self {
        self.operands = ((op as u32) << 24) | ((a as u32) << 16) | ((b as u32) << 8);
        self
    }

    /// BinSlotImmJmpf: [31:24] op, [23:16] slot, [15:0] pool index.
    pub fn with_bin_slot_imm_jmpf(mut self, op: u8, slot: u8, pool_idx: u16) -> Self {
        self.operands = ((op as u32) << 24) | ((slot as u32) << 16) | (pool_idx as u32);
        self
    }

    pub fn bin_slot_imm_jmpf_parts(&self) -> (u8, usize, usize) {
        let o = self.operands;
        (
            (o >> 24) as u8,
            ((o >> 16) & 0xFF) as usize,
            (o & 0xFFFF) as usize,
        )
    }

    /// LogNotJmpf direct target: [15:0] PC (bit 16 clear).
    pub fn with_log_not_jmpf(mut self, target: u16) -> Self {
        self.operands = target as u32;
        self
    }

    /// LogNotJmpf pool target: bit 16 set, [15:0] pool index → absolute PC.
    pub fn with_log_not_jmpf_pool(mut self, pool_idx: u16) -> Self {
        self.operands = (1u32 << 16) | (pool_idx as u32);
        self
    }

    pub fn log_not_jmpf_target(&self) -> usize {
        (self.operands & 0xFFFF) as usize
    }

    pub fn log_not_jmpf_is_pool(&self) -> bool {
        (self.operands & (1u32 << 16)) != 0
    }

    pub fn bin_slot_slot_parts(&self) -> (u8, usize, usize) {
        let o = self.operands;
        (
            (o >> 24) as u8,
            ((o >> 16) & 0xFF) as usize,
            ((o >> 8) & 0xFF) as usize,
        )
    }

    /// BinSlotSlotJmpf: [31:24] op, [23:16] a, [15:0] pool index.
    pub fn with_bin_slot_slot_jmpf(mut self, op: u8, a: u8, pool_idx: u16) -> Self {
        self.operands = ((op as u32) << 24) | ((a as u32) << 16) | (pool_idx as u32);
        self
    }

    pub fn bin_slot_slot_jmpf_parts(&self) -> (u8, usize, usize) {
        let o = self.operands;
        (
            (o >> 24) as u8,
            ((o >> 16) & 0xFF) as usize,
            (o & 0xFFFF) as usize,
        )
    }

    /// BinSlotSlotConstJmpf: [31:24] bin_op, [23:16] a, [15:0] descriptor pool index.
    pub fn with_bin_slot_slot_const_jmpf(mut self, bin_op: u8, a: u8, pool_idx: u16) -> Self {
        self.operands = ((bin_op as u32) << 24) | ((a as u32) << 16) | (pool_idx as u32);
        self
    }

    pub fn bin_slot_slot_const_jmpf_parts(&self) -> (u8, usize, usize) {
        let o = self.operands;
        (
            (o >> 24) as u8,
            ((o >> 16) & 0xFF) as usize,
            (o & 0xFFFF) as usize,
        )
    }

    /// Pack descriptor for [`Instruction::BinSlotSlotConstJmpf`].
    pub fn pack_bin_slot_slot_const_jmpf_desc(
        b: u8,
        cmp_op: u8,
        float_pool_idx: u16,
        target: u32,
    ) -> u64 {
        (b as u64)
            | ((cmp_op as u64) << 8)
            | ((float_pool_idx as u64) << 16)
            | ((target as u64) << 32)
    }

    /// Unpack descriptor for [`Instruction::BinSlotSlotConstJmpf`].
    pub fn unpack_bin_slot_slot_const_jmpf_desc(packed: u64) -> (u8, u8, usize, usize) {
        (
            packed as u8,
            (packed >> 8) as u8,
            ((packed >> 16) & 0xFFFF) as usize,
            (packed >> 32) as usize,
        )
    }

    /// BinSlotImmStore: [31:24] op, [23:16] src, [15:0] pool index.
    pub fn with_bin_slot_imm_store(mut self, op: u8, src: u8, pool_idx: u16) -> Self {
        self.operands = ((op as u32) << 24) | ((src as u32) << 16) | (pool_idx as u32);
        self
    }

    pub fn bin_slot_imm_store_parts(&self) -> (u8, usize, usize) {
        let o = self.operands;
        (
            (o >> 24) as u8,
            ((o >> 16) & 0xFF) as usize,
            (o & 0xFFFF) as usize,
        )
    }

    /// BinSlotSlotStore: [31:24] op, [23:16] a, [15:8] b, [7:0] dest.
    pub fn with_bin_slot_slot_store(mut self, op: u8, a: u8, b: u8, dest: u8) -> Self {
        self.operands =
            ((op as u32) << 24) | ((a as u32) << 16) | ((b as u32) << 8) | (dest as u32);
        self
    }

    pub fn bin_slot_slot_store_parts(&self) -> (u8, usize, usize, usize) {
        let o = self.operands;
        (
            (o >> 24) as u8,
            ((o >> 16) & 0xFF) as usize,
            ((o >> 8) & 0xFF) as usize,
            (o & 0xFF) as usize,
        )
    }

    /// INC/DEC: [31:3] slot, [2] prefix, [1] float.
    pub fn with_inc_dec(mut self, slot: u32, prefix: bool, is_float: bool) -> Self {
        self.operands = (slot << 3) | ((prefix as u32) << 2) | ((is_float as u32) << 1);
        self
    }

    pub fn inc_dec_parts(&self) -> (usize, bool, bool) {
        let o = self.operands;
        ((o >> 3) as usize, (o & 0b100) != 0, (o & 0b010) != 0)
    }

    /// Packed LOAD/STORE: `[31:24]=n` (`1..=3`), `[23:16]=s2`, `[15:8]=s1`, `[7:0]=s0`.
    pub fn with_load_store_packed(mut self, n: u8, s0: u8, s1: u8, s2: u8) -> Self {
        debug_assert!((1..=3).contains(&n), "packed LOAD/STORE n must be 1..=3");
        self.operands = ((n as u32) << 24) | ((s2 as u32) << 16) | ((s1 as u32) << 8) | (s0 as u32);
        self
    }

    /// Wide single-slot LOAD/STORE escape: `n==0`, slot in low 24 bits.
    pub fn with_load_store_wide(mut self, slot: u32) -> Self {
        debug_assert!(slot <= 0x00FF_FFFF, "LOAD/STORE wide slot exceeds 24 bits");
        self.operands = slot & 0x00FF_FFFF;
        self
    }

    /// Single-slot LOAD/STORE: `n=1` when `slot <= 255`, else wide (`n=0`).
    pub fn with_load_store_slot(self, slot: u32) -> Self {
        if slot > 255 {
            self.with_load_store_wide(slot)
        } else {
            self.with_load_store_packed(1, slot as u8, 0, 0)
        }
    }

    /// Slot count for packed LOAD/STORE (`n==0` → 1).
    pub fn load_store_count(&self) -> usize {
        let n = (self.operands >> 24) as u8;
        if n == 0 { 1 } else { n as usize }
    }

    /// Slot at index `i` within a packed LOAD/STORE (`i < load_store_count()`).
    pub fn load_store_slot_at(&self, i: usize) -> u32 {
        let n = (self.operands >> 24) as u8;
        if n == 0 {
            debug_assert!(i == 0);
            return self.operands & 0x00FF_FFFF;
        }
        match i {
            0 => self.operands & 0xFF,
            1 => (self.operands >> 8) & 0xFF,
            2 => (self.operands >> 16) & 0xFF,
            _ => unreachable!("LOAD/STORE slot index out of range"),
        }
    }

    /// Single-slot LOAD/STORE (`n==0` or `n==1`); `None` when `n > 1`.
    pub fn load_store_single_slot(&self) -> Option<u32> {
        let n = (self.operands >> 24) as u8;
        match n {
            0 => Some(self.operands & 0x00FF_FFFF),
            1 => Some(self.operands & 0xFF),
            _ => None,
        }
    }

    /// Packed parts `(n, s0, s1, s2)`. For `n==0`, returns `(0, low24 as u8 truncated, 0, 0)` —
    /// prefer [`load_store_single_slot`] / [`load_store_slot_at`] for wide slots.
    pub fn load_store_parts(&self) -> (u8, u8, u8, u8) {
        let o = self.operands;
        (
            (o >> 24) as u8,
            (o & 0xFF) as u8,
            ((o >> 8) & 0xFF) as u8,
            ((o >> 16) & 0xFF) as u8,
        )
    }
}

impl ArchivedByte {
    pub const POOL_FLAG: u32 = Byte::POOL_FLAG;

    pub fn new(bytecode: ArchivedInstruction) -> Self {
        Self {
            bytecode,
            _pad: [0; 3],
            operands: Default::default(),
        }
    }

    pub fn with_operand_u32(mut self, operand: u32) -> Self {
        self.operands = operand.into();
        self
    }

    pub fn with_call_packed(mut self, arity: u32, target: u32) -> Self {
        self.operands = ((arity << 24) | (target & 0xFFFFFF)).into();
        self
    }

    pub fn with_const_inline(mut self, value: i32) -> Self {
        self.operands = (value as u32).into();
        self
    }

    pub fn with_operands_u16(mut self, operands: [u16; 2]) -> Self {
        let mut operand: u32 = 0;
        operand ^= operands[0] as u32;
        operand <<= 16;
        operand ^= operands[1] as u32;
        self.operands = operand.into();
        self
    }

    pub fn with_value(mut self, value: Value) -> Self {
        let raw = value.raw() as u64;
        let v = raw as i64;
        if v >= i32::MIN as i64 && v <= i32::MAX as i64 {
            self.operands = (v as i32 as u32).into();
        } else {
            self.operands = Self::POOL_FLAG.into();
        }
        self
    }

    pub fn with_value_u32(mut self, v: u32) -> Self {
        self.operands = v.into();
        self
    }

    pub fn value_u32(&self) -> u32 {
        u32::from(self.operands) & 0xFFFFFF
    }

    pub fn bytecode(&self) -> &ArchivedInstruction {
        &self.bytecode
    }

    pub fn operand_u32(&self) -> u32 {
        self.operands.into()
    }

    pub fn operand_u16(&self, index: usize) -> u16 {
        match index {
            0 => (self.operands >> 16) as u16,
            1 => ((self.operands << 16) >> 16) as u16,
            _ => unreachable!("Unable to use larger index when using u32 operands"),
        }
    }

    pub fn constant(&self, pool: &[u64]) -> u64 {
        let op: u32 = self.operands.into();
        if op & Self::POOL_FLAG != 0 {
            let idx = (op & !Self::POOL_FLAG) as usize;
            crate::promise!(idx < pool.len());
            unsafe { *pool.get_unchecked(idx) }
        } else {
            op as i32 as i64 as u64
        }
    }

    pub fn call_parts(&self) -> (usize, usize) {
        let op: u32 = self.operands.into();
        ((op >> 24) as usize, (op & 0xFFFFFF) as usize)
    }

    pub fn jump_if_match_target(&self, pool: &[u64]) -> usize {
        let op: u32 = self.operands.into();
        let idx = (op & 0xFFFF) as usize;
        crate::promise!(idx < pool.len());
        unsafe { *pool.get_unchecked(idx) as usize }
    }

    pub fn bin_slot_imm_parts(&self) -> (u8, usize, i64) {
        let o: u32 = self.operands.into();
        (
            (o >> 24) as u8,
            ((o >> 16) & 0xFF) as usize,
            (o & 0xFFFF) as u16 as i16 as i64,
        )
    }

    pub fn with_bin_slot_imm(mut self, op: u8, slot: u8, imm: i16) -> Self {
        let packed = ((op as u32) << 24) | ((slot as u32) << 16) | (imm as u16 as u32);
        self.operands = packed.into();
        self
    }

    pub fn cmp_jmpf_parts(&self) -> (u8, usize) {
        let o: u32 = self.operands.into();
        ((o >> 24) as u8, (o & 0xFFFF) as usize)
    }

    pub fn cmp_jmpf_is_pool(&self) -> bool {
        (u32::from(self.operands) & (1u32 << 16)) != 0
    }

    pub fn with_cmp_jmpf(mut self, op: u8, target: u16) -> Self {
        self.operands = (((op as u32) << 24) | (target as u32)).into();
        self
    }

    pub fn with_cmp_jmpf_pool(mut self, op: u8, pool_idx: u16) -> Self {
        self.operands = (((op as u32) << 24) | (1u32 << 16) | (pool_idx as u32)).into();
        self
    }

    pub fn bin_return_op(&self) -> u8 {
        (u32::from(self.operands) >> 24) as u8
    }

    pub fn with_bin_return(mut self, op: u8) -> Self {
        self.operands = ((op as u32) << 24).into();
        self
    }

    pub fn bin_slot_slot_parts(&self) -> (u8, usize, usize) {
        let o: u32 = self.operands.into();
        (
            (o >> 24) as u8,
            ((o >> 16) & 0xFF) as usize,
            ((o >> 8) & 0xFF) as usize,
        )
    }

    pub fn with_bin_slot_slot(mut self, op: u8, a: u8, b: u8) -> Self {
        self.operands = (((op as u32) << 24) | ((a as u32) << 16) | ((b as u32) << 8)).into();
        self
    }

    pub fn with_bin_slot_imm_jmpf(mut self, op: u8, slot: u8, pool_idx: u16) -> Self {
        let packed = ((op as u32) << 24) | ((slot as u32) << 16) | (pool_idx as u32);
        self.operands = packed.into();
        self
    }

    pub fn bin_slot_imm_jmpf_parts(&self) -> (u8, usize, usize) {
        let o: u32 = self.operands.into();
        (
            (o >> 24) as u8,
            ((o >> 16) & 0xFF) as usize,
            (o & 0xFFFF) as usize,
        )
    }

    pub fn with_log_not_jmpf(mut self, target: u16) -> Self {
        self.operands = (target as u32).into();
        self
    }

    pub fn with_log_not_jmpf_pool(mut self, pool_idx: u16) -> Self {
        self.operands = ((1u32 << 16) | (pool_idx as u32)).into();
        self
    }

    pub fn log_not_jmpf_target(&self) -> usize {
        (u32::from(self.operands) & 0xFFFF) as usize
    }

    pub fn log_not_jmpf_is_pool(&self) -> bool {
        (u32::from(self.operands) & (1u32 << 16)) != 0
    }

    pub fn with_bin_slot_slot_jmpf(mut self, op: u8, a: u8, pool_idx: u16) -> Self {
        let packed = ((op as u32) << 24) | ((a as u32) << 16) | (pool_idx as u32);
        self.operands = packed.into();
        self
    }

    pub fn bin_slot_slot_jmpf_parts(&self) -> (u8, usize, usize) {
        let o: u32 = self.operands.into();
        (
            (o >> 24) as u8,
            ((o >> 16) & 0xFF) as usize,
            (o & 0xFFFF) as usize,
        )
    }

    pub fn with_bin_slot_slot_const_jmpf(mut self, bin_op: u8, a: u8, pool_idx: u16) -> Self {
        let packed = ((bin_op as u32) << 24) | ((a as u32) << 16) | (pool_idx as u32);
        self.operands = packed.into();
        self
    }

    pub fn bin_slot_slot_const_jmpf_parts(&self) -> (u8, usize, usize) {
        let o: u32 = self.operands.into();
        (
            (o >> 24) as u8,
            ((o >> 16) & 0xFF) as usize,
            (o & 0xFFFF) as usize,
        )
    }

    pub fn bin_slot_imm_store_parts(&self) -> (u8, usize, usize) {
        let o: u32 = self.operands.into();
        (
            (o >> 24) as u8,
            ((o >> 16) & 0xFF) as usize,
            (o & 0xFFFF) as usize,
        )
    }

    pub fn with_bin_slot_imm_store(mut self, op: u8, src: u8, pool_idx: u16) -> Self {
        let packed = ((op as u32) << 24) | ((src as u32) << 16) | (pool_idx as u32);
        self.operands = packed.into();
        self
    }

    pub fn with_bin_slot_slot_store(mut self, op: u8, a: u8, b: u8, dest: u8) -> Self {
        let packed = ((op as u32) << 24) | ((a as u32) << 16) | ((b as u32) << 8) | (dest as u32);
        self.operands = packed.into();
        self
    }

    pub fn bin_slot_slot_store_parts(&self) -> (u8, usize, usize, usize) {
        let o: u32 = self.operands.into();
        (
            (o >> 24) as u8,
            ((o >> 16) & 0xFF) as usize,
            ((o >> 8) & 0xFF) as usize,
            (o & 0xFF) as usize,
        )
    }

    pub fn inc_dec_parts(&self) -> (usize, bool, bool) {
        let o: u32 = self.operands.into();
        ((o >> 3) as usize, (o & 0b100) != 0, (o & 0b010) != 0)
    }

    pub fn with_inc_dec(mut self, slot: u32, prefix: bool, is_float: bool) -> Self {
        self.operands = ((slot << 3) | ((prefix as u32) << 2) | ((is_float as u32) << 1)).into();
        self
    }

    /// Packed LOAD/STORE: `[31:24]=n` (`1..=3`), `[23:16]=s2`, `[15:8]=s1`, `[7:0]=s0`.
    pub fn with_load_store_packed(mut self, n: u8, s0: u8, s1: u8, s2: u8) -> Self {
        debug_assert!((1..=3).contains(&n), "packed LOAD/STORE n must be 1..=3");
        let packed = ((n as u32) << 24) | ((s2 as u32) << 16) | ((s1 as u32) << 8) | (s0 as u32);
        self.operands = packed.into();
        self
    }

    /// Wide single-slot LOAD/STORE escape: `n==0`, slot in low 24 bits.
    pub fn with_load_store_wide(mut self, slot: u32) -> Self {
        debug_assert!(slot <= 0x00FF_FFFF, "LOAD/STORE wide slot exceeds 24 bits");
        self.operands = (slot & 0x00FF_FFFF).into();
        self
    }

    /// Single-slot LOAD/STORE: `n=1` when `slot <= 255`, else wide (`n=0`).
    pub fn with_load_store_slot(self, slot: u32) -> Self {
        if slot > 255 {
            self.with_load_store_wide(slot)
        } else {
            self.with_load_store_packed(1, slot as u8, 0, 0)
        }
    }

    /// Slot count for packed LOAD/STORE (`n==0` → 1).
    pub fn load_store_count(&self) -> usize {
        let o: u32 = self.operands.into();
        let n = (o >> 24) as u8;
        if n == 0 { 1 } else { n as usize }
    }

    /// Slot at index `i` within a packed LOAD/STORE.
    pub fn load_store_slot_at(&self, i: usize) -> u32 {
        let o: u32 = self.operands.into();
        let n = (o >> 24) as u8;
        if n == 0 {
            debug_assert!(i == 0);
            return o & 0x00FF_FFFF;
        }
        match i {
            0 => o & 0xFF,
            1 => (o >> 8) & 0xFF,
            2 => (o >> 16) & 0xFF,
            _ => unreachable!("LOAD/STORE slot index out of range"),
        }
    }

    /// Single-slot LOAD/STORE (`n==0` or `n==1`); `None` when `n > 1`.
    pub fn load_store_single_slot(&self) -> Option<u32> {
        let o: u32 = self.operands.into();
        let n = (o >> 24) as u8;
        match n {
            0 => Some(o & 0x00FF_FFFF),
            1 => Some(o & 0xFF),
            _ => None,
        }
    }
}

#[cfg(debug_assertions)]
use std::fmt::Debug;

#[cfg(debug_assertions)]
impl Debug for Byte {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}({:?})", self.bytecode, self.operands)
    }
}

#[cfg(debug_assertions)]
impl std::fmt::Display for Instruction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", *self as u8)
    }
}
#[cfg(debug_assertions)]
impl Debug for ArchivedByte {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}({:?})", self.bytecode as u8, self.operands)
    }
}

#[cfg(debug_assertions)]
impl std::fmt::Display for ArchivedInstruction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", *self as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_packed_round_trips_arity_and_target() {
        let b = Byte::new(Instruction::CALL).with_call_packed(3, 0x123456);
        assert_eq!(b.call_parts(), (3, 0x123456));
        assert_eq!(b.value_u32(), 0x123456);
    }

    #[test]
    fn operands_u16_pack_and_unpack() {
        let b = Byte::new(Instruction::MakeEnum).with_operands_u16([7, 2]);
        assert_eq!(b.operand_u16(0), 7);
        assert_eq!(b.operand_u16(1), 2);
        assert_eq!(b.operand_u32(), (7u32 << 16) | 2);
    }

    #[test]
    fn const_inline_and_pool_constant_resolution() {
        // Inline CONST uses the high bit as POOL_FLAG, so only non-negative
        // i32 values that clear that bit are safe to encode inline.
        let inline = Byte::new(Instruction::CONST).with_const_inline(5);
        assert_eq!(inline.constant(&[]) as i64, 5);

        let pool = Byte::new(Instruction::CONST).with_const_pool(1);
        assert_ne!(pool.operand_u32() & Byte::POOL_FLAG, 0);
        let constants = [0u64, 1.5f64.to_bits()];
        assert_eq!(pool.constant(&constants), 1.5f64.to_bits());
    }

    #[test]
    fn bin_slot_imm_sign_extends_immediate() {
        let b = Byte::new(Instruction::BinSlotImm).with_bin_slot_imm(Instruction::ADD as u8, 3, -7);
        let (op, slot, imm) = b.bin_slot_imm_parts();
        assert_eq!(op, Instruction::ADD as u8);
        assert_eq!(slot, 3);
        assert_eq!(imm, -7);
    }

    #[test]
    fn bin_slot_slot_and_cmp_jmpf_and_bin_return_pack() {
        let slot =
            Byte::new(Instruction::BinSlotSlot).with_bin_slot_slot(Instruction::MUL as u8, 1, 2);
        assert_eq!(slot.bin_slot_slot_parts(), (Instruction::MUL as u8, 1, 2));

        let cmp = Byte::new(Instruction::CmpJmpf).with_cmp_jmpf(Instruction::EQ as u8, 99);
        assert_eq!(cmp.cmp_jmpf_parts(), (Instruction::EQ as u8, 99));

        let ret = Byte::new(Instruction::BinReturn).with_bin_return(Instruction::SUB as u8);
        assert_eq!(ret.bin_return_op(), Instruction::SUB as u8);
    }

    #[test]
    fn fused_jmpf_and_inc_dec_parts() {
        let j = Byte::new(Instruction::BinSlotImmJmpf).with_bin_slot_imm_jmpf(
            Instruction::LEQ as u8,
            0,
            4,
        );
        assert_eq!(j.bin_slot_imm_jmpf_parts(), (Instruction::LEQ as u8, 0, 4));

        let n = Byte::new(Instruction::LogNotJmpf).with_log_not_jmpf(12);
        assert_eq!(n.log_not_jmpf_target(), 12);

        let inc = Byte::new(Instruction::INC).with_inc_dec(5, true, false);
        assert_eq!(inc.inc_dec_parts(), (5, true, false));
        let dec = Byte::new(Instruction::DEC).with_inc_dec(2, false, true);
        assert_eq!(dec.inc_dec_parts(), (2, false, true));
    }

    #[test]
    fn instruction_from_u8_covers_last_appended_variant() {
        // ARCHIVE stability: last variant must remain decodable (keep in sync
        // with machine release `promise!` ceiling).
        let last = Instruction::StoreIndexUnchecked as u8;
        let decoded: Instruction = last.into();
        assert_eq!(decoded as u8, last);
    }

    #[test]
    fn mnemonic_covers_first_and_last_variants() {
        assert_eq!(Instruction::HALT.mnemonic(), "HALT");
        assert_eq!(Instruction::Seek.mnemonic(), "Seek");
        assert_eq!(Instruction::ReturnPair.mnemonic(), "ReturnPair");
        assert_eq!(Instruction::HostInvokeNiche.mnemonic(), "HostInvokeNiche");
        assert_eq!(Instruction::FloatChainStore.mnemonic(), "FloatChainStore");
        assert_eq!(
            Instruction::BinSlotSlotConstJmpf.mnemonic(),
            "BinSlotSlotConstJmpf"
        );
        assert_eq!(Instruction::NEGF.mnemonic(), "NEGF");
        assert_eq!(Instruction::InitTyped.mnemonic(), "InitTyped");
        assert_eq!(Instruction::BinSlotSlotConstJmpt.mnemonic(), "BinSlotSlotConstJmpt");
        assert_eq!(Instruction::BinSlotSlotStore.mnemonic(), "BinSlotSlotStore");
        assert_eq!(Instruction::BinSlotImmStore.mnemonic(), "BinSlotImmStore");
        assert_eq!(Instruction::TailCall.mnemonic(), "TailCall");
    }

    #[test]
    fn bin_slot_slot_const_jmpf_pack() {
        let b = Byte::new(Instruction::BinSlotSlotConstJmpf).with_bin_slot_slot_const_jmpf(
            Instruction::ADDF as u8,
            10,
            3,
        );
        assert_eq!(
            b.bin_slot_slot_const_jmpf_parts(),
            (Instruction::ADDF as u8, 10, 3)
        );
        let desc = Byte::pack_bin_slot_slot_const_jmpf_desc(11, Instruction::GTF as u8, 6, 815);
        let (slot_b, cmp, fidx, target) = Byte::unpack_bin_slot_slot_const_jmpf_desc(desc);
        assert_eq!(slot_b, 11);
        assert_eq!(cmp, Instruction::GTF as u8);
        assert_eq!(fidx, 6);
        assert_eq!(target, 815);
    }

    #[test]
    fn bin_slot_slot_jmpf_and_pool_cmp_log_not_pack() {
        let j = Byte::new(Instruction::BinSlotSlotJmpf).with_bin_slot_slot_jmpf(
            Instruction::LE as u8,
            1,
            7,
        );
        assert_eq!(j.bin_slot_slot_jmpf_parts(), (Instruction::LE as u8, 1, 7));

        let cmp = Byte::new(Instruction::CmpJmpf).with_cmp_jmpf_pool(Instruction::EQ as u8, 3);
        assert!(cmp.cmp_jmpf_is_pool());
        assert_eq!(cmp.cmp_jmpf_parts(), (Instruction::EQ as u8, 3));

        let n = Byte::new(Instruction::LogNotJmpf).with_log_not_jmpf_pool(9);
        assert!(n.log_not_jmpf_is_pool());
        assert_eq!(n.log_not_jmpf_target(), 9);
    }

    #[test]
    fn bin_slot_imm_store_and_slot_store_pack() {
        let imm = Byte::new(Instruction::BinSlotImmStore).with_bin_slot_imm_store(
            Instruction::ADD as u8,
            2,
            5,
        );
        assert_eq!(
            imm.bin_slot_imm_store_parts(),
            (Instruction::ADD as u8, 2, 5)
        );

        let ss = Byte::new(Instruction::BinSlotSlotStore).with_bin_slot_slot_store(
            Instruction::BITAND as u8,
            1,
            3,
            4,
        );
        assert_eq!(
            ss.bin_slot_slot_store_parts(),
            (Instruction::BITAND as u8, 1, 3, 4)
        );
    }

    #[test]
    fn load_store_packed_round_trip() {
        let load3 = Byte::new(Instruction::LOAD).with_load_store_packed(3, 0, 1, 2);
        assert_eq!(load3.load_store_count(), 3);
        assert_eq!(load3.load_store_slot_at(0), 0);
        assert_eq!(load3.load_store_slot_at(1), 1);
        assert_eq!(load3.load_store_slot_at(2), 2);
        assert_eq!(load3.load_store_parts(), (3, 0, 1, 2));
        assert!(load3.load_store_single_slot().is_none());

        let store2 = Byte::new(Instruction::STORE).with_load_store_packed(2, 4, 5, 0);
        assert_eq!(store2.load_store_count(), 2);
        assert_eq!(store2.load_store_slot_at(0), 4);
        assert_eq!(store2.load_store_slot_at(1), 5);
        assert_eq!(store2.load_store_parts(), (2, 4, 5, 0));

        let single = Byte::new(Instruction::LOAD).with_load_store_slot(7);
        assert_eq!(single.load_store_single_slot(), Some(7));
        assert_eq!(single.load_store_count(), 1);
        assert_eq!(single.load_store_parts().0, 1);

        let wide = Byte::new(Instruction::STORE).with_load_store_slot(300);
        assert_eq!(wide.load_store_single_slot(), Some(300));
        assert_eq!(wide.load_store_parts().0, 0);
        assert_eq!(wide.operand_u32() & 0x00FF_FFFF, 300);
    }
}
