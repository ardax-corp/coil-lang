//! SSA instructions for the numeric MIR subset.

use super::ty::MirTy;

/// SSA value name. Dense, function-local.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ValueId(pub u32);

impl ValueId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

impl std::fmt::Display for ValueId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "v{}", self.0)
    }
}

/// Basic-block id. Dense, function-local.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockId(pub u32);

impl BlockId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

impl std::fmt::Display for BlockId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "bb{}", self.0)
    }
}

/// Named mutable source local (IL slot) during SSA construction.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LocalId(pub u32);

/// Typed immediate.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum MirConst {
    I32(i32),
    I64(i64),
    /// IEEE bits (Eq/Hash-friendly).
    F32(u32),
    F64(u64),
    Bool(bool),
}

impl MirConst {
    pub fn ty(self) -> MirTy {
        match self {
            Self::I32(_) => MirTy::I32,
            Self::I64(_) => MirTy::I64,
            Self::F32(_) => MirTy::F32,
            Self::F64(_) => MirTy::F64,
            Self::Bool(_) => MirTy::Bool,
        }
    }

    #[allow(dead_code)]
    pub fn f32(v: f32) -> Self {
        Self::F32(v.to_bits())
    }

    pub fn f64(v: f64) -> Self {
        Self::F64(v.to_bits())
    }

    #[allow(dead_code)]
    pub fn as_f64(self) -> Option<f64> {
        match self {
            Self::F64(b) => Some(f64::from_bits(b)),
            Self::F32(b) => Some(f32::from_bits(b) as f64),
            _ => None,
        }
    }
}

impl Eq for MirConst {}

impl std::hash::Hash for MirConst {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match *self {
            Self::I32(v) => v.hash(state),
            Self::I64(v) => v.hash(state),
            Self::F32(v) => v.hash(state),
            Self::F64(v) => v.hash(state),
            Self::Bool(v) => v.hash(state),
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum MirBinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    BitAnd,
    BitOr,
    Xor,
    Shl,
    Shr,
    /// `**`: an int `**` traps on overflow and on a negative exponent, as
    /// the stack `Pow`.
    Pow,
}

impl MirBinOp {
    pub fn as_str(self, ty: MirTy) -> &'static str {
        match (self, ty.is_float()) {
            (Self::Add, false) => "iadd",
            (Self::Add, true) => "fadd",
            (Self::Sub, false) => "isub",
            (Self::Sub, true) => "fsub",
            (Self::Mul, false) => "imul",
            (Self::Mul, true) => "fmul",
            (Self::Div, false) => "idiv",
            (Self::Div, true) => "fdiv",
            (Self::Rem, false) => "irem",
            (Self::Rem, true) => "frem",
            (Self::BitAnd, _) => "iand",
            (Self::BitOr, _) => "ior",
            (Self::Xor, _) => "ixor",
            (Self::Shl, _) => "ishl",
            (Self::Shr, _) => "ishr",
            (Self::Pow, false) => "ipow",
            (Self::Pow, true) => "fpow",
        }
    }

    pub fn parse(s: &str) -> Option<(Self, bool)> {
        Some(match s {
            "iadd" => (Self::Add, false),
            "fadd" => (Self::Add, true),
            "isub" => (Self::Sub, false),
            "fsub" => (Self::Sub, true),
            "imul" => (Self::Mul, false),
            "fmul" => (Self::Mul, true),
            "idiv" => (Self::Div, false),
            "fdiv" => (Self::Div, true),
            "irem" => (Self::Rem, false),
            "frem" => (Self::Rem, true),
            "iand" => (Self::BitAnd, false),
            "ior" => (Self::BitOr, false),
            "ixor" => (Self::Xor, false),
            "ishl" => (Self::Shl, false),
            "ishr" => (Self::Shr, false),
            "ipow" => (Self::Pow, false),
            "fpow" => (Self::Pow, true),
            _ => return None,
        })
    }

    /// `**` is a float or an `i64` op (no narrow int `**`).
    pub fn takes(self, ty: MirTy) -> bool {
        self != Self::Pow || ty.is_float() || ty == MirTy::I64
    }

    pub fn requires_int(self) -> bool {
        matches!(
            self,
            Self::BitAnd | Self::BitOr | Self::Xor | Self::Shl | Self::Shr
        )
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum MirCmpOp {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
}

impl MirCmpOp {
    pub fn as_str(self, float: bool) -> &'static str {
        match (self, float) {
            (Self::Lt, false) => "icmp.slt",
            (Self::Le, false) => "icmp.sle",
            (Self::Gt, false) => "icmp.sgt",
            (Self::Ge, false) => "icmp.sge",
            (Self::Eq, false) => "icmp.eq",
            (Self::Ne, false) => "icmp.ne",
            (Self::Lt, true) => "fcmp.olt",
            (Self::Le, true) => "fcmp.ole",
            (Self::Gt, true) => "fcmp.ogt",
            (Self::Ge, true) => "fcmp.oge",
            (Self::Eq, true) => "fcmp.oeq",
            (Self::Ne, true) => "fcmp.one",
        }
    }

    pub fn parse(s: &str) -> Option<(Self, bool)> {
        Some(match s {
            "icmp.slt" => (Self::Lt, false),
            "icmp.sle" => (Self::Le, false),
            "icmp.sgt" => (Self::Gt, false),
            "icmp.sge" => (Self::Ge, false),
            "icmp.eq" => (Self::Eq, false),
            "icmp.ne" => (Self::Ne, false),
            "fcmp.olt" => (Self::Lt, true),
            "fcmp.ole" => (Self::Le, true),
            "fcmp.ogt" => (Self::Gt, true),
            "fcmp.oge" => (Self::Ge, true),
            "fcmp.oeq" => (Self::Eq, true),
            "fcmp.one" => (Self::Ne, true),
            _ => return None,
        })
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum MirUnaryOp {
    Neg,
    Not,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum MirCastKind {
    /// `CastIntToFloat` — signed int to IEEE float of matching width or widen.
    IntToFloat,
    /// `i32` → `i64`.
    Sext,
    /// `CastFloatToInt` — `f64` → `i64`, truncating toward zero.
    FloatToInt,
    /// The same word retyped: a heap pointer or the `i64` zero (`None` /
    /// `Ok(())`) as a `NicheOpt` / `NicheRes` word, such a word known to
    /// hold a pointer as `HeapRef`, or a heap, niche or `bool` word moved
    /// through an opaque `i64` pair word and back.
    Word,
}

impl MirCastKind {
    /// Whether a cast of this kind from `from` to `to` is legal.
    pub fn allows(self, from: MirTy, to: MirTy) -> bool {
        match self {
            Self::IntToFloat => from.is_int() && to.is_float(),
            Self::Sext => from == MirTy::I32 && to == MirTy::I64,
            Self::FloatToInt => from == MirTy::F64 && to == MirTy::I64,
            Self::Word => {
                let niche = |t: MirTy| matches!(t, MirTy::NicheOpt | MirTy::NicheRes);
                (niche(to) && matches!(from, MirTy::I64 | MirTy::HeapRef))
                    || (niche(from) && matches!(to, MirTy::HeapRef | MirTy::I64))
                    || matches!((from, to), (MirTy::I64, MirTy::HeapRef) | (MirTy::HeapRef, MirTy::I64))
                    // A `bool` word is 0 or 1: as an `i64` it is the same bits.
                    || matches!((from, to), (MirTy::Bool, MirTy::I64) | (MirTy::I64, MirTy::Bool))
            }
        }
    }
}

/// Value-producing instruction. Phis occupy the prefix of a block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MirInst {
    Const {
        dest: ValueId,
        c: MirConst,
    },
    Bin {
        dest: ValueId,
        op: MirBinOp,
        ty: MirTy,
        lhs: ValueId,
        rhs: ValueId,
    },
    Cmp {
        dest: ValueId,
        op: MirCmpOp,
        ty: MirTy,
        lhs: ValueId,
        rhs: ValueId,
    },
    Unary {
        dest: ValueId,
        op: MirUnaryOp,
        src: ValueId,
    },
    Cast {
        dest: ValueId,
        kind: MirCastKind,
        to: MirTy,
        src: ValueId,
    },
    Phi {
        dest: ValueId,
        ty: MirTy,
        args: Vec<(BlockId, ValueId)>,
    },
    /// HostInvoke edge. W4 math/packed/axpy may emit dense (box at the
    /// edge). I6 types other natives as barriers; impure never hoists.
    HostInvoke {
        dest: ValueId,
        native_id: u16,
        /// `common::HOST_ENUM_LAYOUT_*` packed on reconstruct.
        layout: u8,
        args: Vec<ValueId>,
    },
    /// Dense / LIR `CALL` (COI-291 / B3). Args stay on the operand stack
    /// (LOAD / BinSlotImm convoy). One-word `Entry` uses `dest` only;
    /// two-slot `[payload, tag]` sets `dest_hi`. `STORE` only when a dest
    /// is live in a slot. Same bits as the callee's slots `0..n`.
    Call {
        dest: ValueId,
        dest_hi: Option<ValueId>,
        target: crate::il::Label,
        args: Vec<ValueId>,
    },
    /// `CallIndirect n`: call the function value `callee` (an `ObjFn` /
    /// `PolyFn` heap word) with `args`, pushed in order under it. Dest is
    /// the one word it returns. Opaque: any effect, a GC safepoint in the
    /// callee; never CSE'd or hoisted.
    CallIndirect {
        dest: ValueId,
        callee: ValueId,
        args: Vec<ValueId>,
    },
    /// Taken-edge / last-arm payload of [`Terminator::JumpIfMatch`] or
    /// `Unpack` (I2 / D3). `index` is the declaration-order payload word.
    MatchPayload {
        dest: ValueId,
        scrutinee: ValueId,
        index: u32,
    },
    /// Field of a non-escaping unboxed class (I3). `object` is the SSA
    /// value of slot `base + index` from the local-escape unbox map.
    FieldLoad {
        dest: ValueId,
        object: ValueId,
        base: u32,
        index: u32,
    },
    /// Store into an unboxed class field slot (ctor / rebind / S2j
    /// non-escaping `p.x = …`). Escaping named locals stay fuse-IL.
    FieldStore {
        dest: ValueId,
        src: ValueId,
        base: u32,
        index: u32,
    },
    /// Heap `GetField` / `LoadField` on an escaping object (D1 maps).
    /// `object` is [`MirTy::HeapRef`]. Dest is the field word (not SROA).
    /// `name` is the string key for `GetField`; `LoadField` uses `index`.
    HeapFieldLoad {
        dest: ValueId,
        object: ValueId,
        name: Option<ValueId>,
        index: u32,
    },
    /// Heap `SetField`. Dest is the stored value (VM leaves it TOS).
    /// Indexed slot form uses `index`; named form uses `name`.
    HeapFieldStore {
        dest: ValueId,
        object: ValueId,
        value: ValueId,
        name: Option<ValueId>,
        index: Option<u32>,
    },
    /// Heap index load (S3). `array` is `HeapRef`; `index` is `i64`.
    /// Reconstructs `Index` / `IndexUnchecked` (pin forms lower to the
    /// same unpinned residuals — pin slots do not survive dense Seek).
    Index {
        dest: ValueId,
        array: ValueId,
        index: ValueId,
        unchecked: bool,
    },
    /// Heap index store (S3). Dest is the stored value (VM leaves it TOS).
    StoreIndex {
        dest: ValueId,
        array: ValueId,
        index: ValueId,
        value: ValueId,
        unchecked: bool,
    },
    /// Structural `ArrayLen` (S3). Dest is `i64`.
    ArrayLen {
        dest: ValueId,
        array: ValueId,
    },
    /// `ResumeCoro` (C2b). Handle is [`MirTy::HeapRef`]. Dest is the yield
    /// word. `send` is present when the opcode has_send bit is set.
    ResumeCoro {
        dest: ValueId,
        handle: ValueId,
        send: Option<ValueId>,
    },
    /// `YieldCoro` (or `YieldFromCoro` when `from`): suspend the running
    /// coroutine with `value` (a sub-coroutine handle for `from`). The VM
    /// saves the frame as it stands and resumes at the next instruction,
    /// so this is one instruction, and a GC safepoint as a call is: every
    /// live heap word stays in the frame. With `receive`, dest is the word
    /// the next `resume .. with` sends (stored right after the yield);
    /// otherwise a `bool` unit token.
    Yield {
        dest: ValueId,
        value: ValueId,
        from: bool,
        receive: bool,
    },
    /// `DoneCoro` (C2b). Dest is [`MirTy::Bool`].
    DoneCoro {
        dest: ValueId,
        handle: ValueId,
    },
    /// `LoadStatic slot`: a global static's word. Dest is the static's
    /// word type (a heap word is [`MirTy::HeapRef`]). Reads memory a
    /// [`Self::StoreStatic`] or call may write: never CSE'd or hoisted.
    LoadStatic {
        dest: ValueId,
        slot: u32,
    },
    /// `StoreStatic slot`: write `value` to a global static. Dest is a
    /// `bool` unit token, as [`Self::Print`]. A side effect.
    StoreStatic {
        dest: ValueId,
        slot: u32,
        value: ValueId,
    },
    /// Read home `home` ([`super::MirFunc::homes`]): a frame word past the
    /// body's registers that holds a `defer`'s flag or capture where its
    /// unwind cleanup pad reads it. Never CSE'd or hoisted.
    HomeLoad {
        dest: ValueId,
        home: u16,
    },
    /// Write `value` to home `home`: each definition of a `defer` flag or
    /// capture, so the cleanup pad finds it at any panic. Dest is a `bool`
    /// unit token. A side effect.
    HomeStore {
        dest: ValueId,
        home: u16,
        value: ValueId,
    },
    /// Zero the frame word holding `value` (a spliced heap local past its
    /// last use, HIR `Clear`), so the frame no longer roots its object.
    /// Dest is a `bool` unit token. A side effect; emission writes the zero
    /// only when no value sharing that word is live after it.
    ClearWord {
        dest: ValueId,
        value: ValueId,
    },
    /// Keep `value` live to here: a finalizer object's word, held as long
    /// as HIR emission's frame slot holds it (until the local is rebound
    /// or the function returns), so its `drop` runs no earlier. Dest is a
    /// `bool` unit token. Emits nothing.
    KeepAlive {
        dest: ValueId,
        value: ValueId,
    },
    /// In-place `Vec` / array grow (B6). Dest is the same `HeapRef` identity.
    /// Always a GC safepoint; pair with [`Self::GcBarrier`].
    ArrayPush {
        dest: ValueId,
        array: ValueId,
        value: ValueId,
    },
    /// Heap allocation (I5). Dest is [`MirTy::HeapRef`]. Always a GC
    /// safepoint; pair with [`Self::GcBarrier`]. S2a records live heap
    /// words on the barrier and [`crate::mir::func::MirFunc::gc_roots`].
    /// S2c may emit across this edge when S2b maps exist.
    Alloc {
        dest: ValueId,
        kind: MirAllocKind,
        elems: Vec<ValueId>,
        /// Construction word kinds from the source types, as HIR emission
        /// stamps them: an array's element kind (`MakeArrayK`), or a tuple's
        /// / enum's packed payload kinds (`MakeTupleK` / `MakeEnumK`).
        /// `0` = none recorded; emission then reads tuple / enum kinds off
        /// the element words' MIR types.
        kinds: u8,
    },
    /// GC safepoint / write-barrier (I5). Dest is a `HeapRef` token
    /// (not a second object). `roots` is the S2a live-heap SSA list
    /// (new object plus other live heap words). Not an interpreter map.
    GcBarrier {
        dest: ValueId,
        kind: MirGcKind,
        roots: Vec<ValueId>,
    },
    /// Debugger stop / deopt boundary (I7). Dest is a `bool` token.
    /// Sidecar only — emit skips this inst. Names the edge a later
    /// native tier must leave or pause at.
    Deopt {
        dest: ValueId,
        kind: MirDeoptKind,
        loc: common::DebugLoc,
    },
    /// Const string-table push (I4 / Q9 R1). Dest is [`MirTy::HeapRef`].
    String {
        dest: ValueId,
        idx: u32,
    },
    /// `CodePtr` of the function at `target`: its entry as an `int` word,
    /// as an instance dictionary holds its methods. Dest is
    /// [`MirTy::I64`].
    CodePtr {
        dest: ValueId,
        target: crate::il::Label,
    },
    /// `PRINT` (I4 / Q9 R1). Dest is a `bool` unit token. IO barrier.
    Print {
        dest: ValueId,
        src: ValueId,
    },
    /// `Panic` of the string `msg`: the frame never continues, so the
    /// block ends in [`Terminator::Unreachable`]. Dest is a `bool` unit
    /// token, as [`Self::Print`]. A side effect.
    Panic {
        dest: ValueId,
        msg: ValueId,
    },
    /// `FORMAT n` (I4 / Q9 R1). Dest / `fmt` are [`MirTy::HeapRef`].
    /// Reconstructs the shipped opcode — not a second Format lowering.
    /// Allocating safepoint (R3); pair with [`Self::GcBarrier`].
    Format {
        dest: ValueId,
        fmt: ValueId,
        args: Vec<ValueId>,
    },
    /// `STRINGIFY` (I4 / Q9 R1). Dest is [`MirTy::HeapRef`].
    /// Allocating safepoint (R3); pair with [`Self::GcBarrier`].
    Stringify {
        dest: ValueId,
        src: ValueId,
    },
    /// `UnboxValue tag`: the payload of `src` when it is a box of `tag`
    /// (the default word for another box); any other word as it is. A
    /// dictionary entry's argument comes boxed. Reads only the immutable
    /// box, never allocates.
    Unbox {
        dest: ValueId,
        src: ValueId,
        tag: u32,
    },
    /// `TagArrayKind kind`: stamps the element kind of array `src` (a
    /// precise-reference or packed-byte `Vec`); `dest` is the same array.
    /// Writes the object, never allocates.
    TagArrayKind {
        dest: ValueId,
        src: ValueId,
        kind: u8,
    },
    /// String `==` (`!=` when `ne`): VM `EQ` / `NEQ`, which compare strings
    /// by content. Dest is `bool`; `lhs` / `rhs` are string
    /// [`MirTy::HeapRef`] words. Also a user type's `==` with no `Eq`
    /// instance, which the VM compares structurally: that reads mutable
    /// objects, so it is never CSE'd or hoisted (it is not, as above).
    StrEq {
        dest: ValueId,
        lhs: ValueId,
        rhs: ValueId,
        ne: bool,
    },
}

/// Heap object constructed by [`MirInst::Alloc`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum MirAllocKind {
    Array,
    Tuple,
    /// `InitTyped` (`type_id`, `nfields`). Heap fields are SSA for maps (D1);
    /// dense Object `DenseMakeObject` is D2.
    Object {
        type_id: u32,
        nfields: u32,
    },
    Enum {
        tag: u32,
    },
    /// A payload variant of an enum with `fn drop()`: `MakeEnum` (`tag`)
    /// then `TagEnumType` (`type_id`), so the collector runs its finalizer.
    DropEnum {
        tag: u32,
        type_id: u32,
    },
    /// `MakeDict` — elems are interleaved value/name words (2 per field).
    Dict,
    /// `DictEntries`: dict heapref → array of `(string, V)` pairs (C2b).
    DictEntries,
    /// `MakeCoro`: args → coroutine handle (C2b). Target is the async body.
    Coro {
        target: crate::il::Label,
    },
    /// `BoxValue` (`tag`): one word into a boxed carrier, as an operator
    /// instance method takes its operands.
    Boxed {
        tag: u32,
    },
    /// `MakeFn`: a function value of the code at `target`, elems its
    /// captures then its filled parameters (the `Const mask` fill mask and
    /// `CodePtr` pushed after them). `operand` is the packed `MakeFn`
    /// operand; `mask` has a bit per filled parameter (0: none, a closure
    /// or a named function's value; a prefix: a partial application).
    Fn {
        target: crate::il::Label,
        operand: u32,
        mask: u32,
    },
    /// `MakePolyFn`: a generic function's value with no dictionaries.
    PolyFn {
        target: crate::il::Label,
    },
}

impl MirAllocKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Array => "array",
            Self::Tuple => "tuple",
            Self::Object { .. } => "object",
            Self::Enum { .. } => "enum",
            Self::DropEnum { .. } => "dropenum",
            Self::Dict => "dict",
            Self::DictEntries => "dictentries",
            Self::Coro { .. } => "coro",
            Self::Boxed { .. } => "boxed",
            Self::Fn { .. } => "fn",
            Self::PolyFn { .. } => "polyfn",
        }
    }
}

/// Placeholder GC coordination on an alloc / future call edge (I5).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum MirGcKind {
    Safepoint,
    Write,
}

impl MirGcKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Safepoint => "safepoint",
            Self::Write => "write",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "safepoint" => Self::Safepoint,
            "write" => Self::Write,
            _ => return None,
        })
    }
}

/// Stop / leave-specialized edge (I7). Not a bytecode opcode.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum MirDeoptKind {
    /// Interpreter may pause here (line / `stepi` boundary).
    Stop,
    /// Leave specialized or native code; resume fuse-IL at this edge.
    Deopt,
}

impl MirDeoptKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stop => "stop",
            Self::Deopt => "deopt",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "stop" => Self::Stop,
            "deopt" => Self::Deopt,
            _ => return None,
        })
    }
}

impl MirInst {
    pub fn dest(&self) -> ValueId {
        match *self {
            Self::Const { dest, .. }
            | Self::Bin { dest, .. }
            | Self::Cmp { dest, .. }
            | Self::Unary { dest, .. }
            | Self::Cast { dest, .. }
            | Self::Phi { dest, .. }
            | Self::HostInvoke { dest, .. }
            | Self::Call { dest, .. }
            | Self::CallIndirect { dest, .. }
            | Self::MatchPayload { dest, .. }
            | Self::FieldLoad { dest, .. }
            | Self::FieldStore { dest, .. }
            | Self::HeapFieldLoad { dest, .. }
            | Self::HeapFieldStore { dest, .. }
            | Self::Index { dest, .. }
            | Self::StoreIndex { dest, .. }
            | Self::ArrayLen { dest, .. }
            | Self::ResumeCoro { dest, .. }
            | Self::Yield { dest, .. }
            | Self::DoneCoro { dest, .. }
            | Self::LoadStatic { dest, .. }
            | Self::StoreStatic { dest, .. }
            | Self::HomeLoad { dest, .. }
            | Self::HomeStore { dest, .. }
            | Self::ClearWord { dest, .. }
            | Self::KeepAlive { dest, .. }
            | Self::ArrayPush { dest, .. }
            | Self::Alloc { dest, .. }
            | Self::GcBarrier { dest, .. }
            | Self::Deopt { dest, .. }
            | Self::String { dest, .. }
            | Self::CodePtr { dest, .. }
            | Self::Print { dest, .. }
            | Self::Panic { dest, .. }
            | Self::Format { dest, .. }
            | Self::Stringify { dest, .. }
            | Self::Unbox { dest, .. }
            | Self::TagArrayKind { dest, .. }
            | Self::StrEq { dest, .. } => dest,
        }
    }

    /// `dest` plus a two-slot CALL tag / second word when present.
    pub fn dests(&self) -> impl Iterator<Item = ValueId> {
        let hi = match *self {
            Self::Call {
                dest_hi: Some(h), ..
            } => Some(h),
            _ => None,
        };
        std::iter::once(self.dest()).chain(hi)
    }

    /// Allocation or GC safepoint — specialize / native must not cross.
    pub fn is_gc_edge(&self) -> bool {
        matches!(
            self,
            Self::Alloc { .. }
                | Self::ArrayPush { .. }
                | Self::Format { .. }
                | Self::Stringify { .. }
                | Self::GcBarrier { .. }
        )
    }

    /// Explicit I7 stop / deopt inst.
    pub fn is_deopt_edge(&self) -> bool {
        matches!(self, Self::Deopt { .. })
    }

    pub fn is_phi(&self) -> bool {
        matches!(self, Self::Phi { .. })
    }

    pub fn operands(&self) -> Vec<ValueId> {
        match self {
            Self::Const { .. } => Vec::new(),
            Self::Bin { lhs, rhs, .. } | Self::Cmp { lhs, rhs, .. } | Self::StrEq { lhs, rhs, .. } => vec![*lhs, *rhs],
            Self::Unary { src, .. } | Self::Cast { src, .. } => vec![*src],
            Self::Phi { args, .. } => args.iter().map(|(_, v)| *v).collect(),
            Self::HostInvoke { args, .. } | Self::Call { args, .. } => args.clone(),
            Self::CallIndirect { callee, args, .. } => args.iter().copied().chain([*callee]).collect(),
            Self::MatchPayload { scrutinee, .. } => vec![*scrutinee],
            Self::FieldLoad { object, .. } => vec![*object],
            Self::FieldStore { src, .. } => vec![*src],
            Self::HeapFieldLoad {
                object, name, ..
            } => {
                let mut v = vec![*object];
                if let Some(n) = name {
                    v.push(*n);
                }
                v
            }
            Self::HeapFieldStore {
                object,
                value,
                name,
                ..
            } => {
                let mut v = vec![*object, *value];
                if let Some(n) = name {
                    v.push(*n);
                }
                v
            }
            Self::Index { array, index, .. } => vec![*array, *index],
            Self::StoreIndex {
                array,
                index,
                value,
                ..
            } => vec![*array, *index, *value],
            Self::ArrayLen { array, .. } => vec![*array],
            Self::ResumeCoro { handle, send, .. } => {
                let mut v = vec![*handle];
                if let Some(s) = send {
                    v.push(*s);
                }
                v
            }
            Self::Yield { value, .. } => vec![*value],
            Self::DoneCoro { handle, .. } => vec![*handle],
            Self::LoadStatic { .. } | Self::HomeLoad { .. } => Vec::new(),
            Self::StoreStatic { value, .. } | Self::HomeStore { value, .. } | Self::ClearWord { value, .. } | Self::KeepAlive { value, .. } => vec![*value],
            Self::ArrayPush { array, value, .. } => vec![*array, *value],
            Self::Alloc { elems, .. } => elems.clone(),
            Self::GcBarrier { roots, .. } => roots.clone(),
            Self::Deopt { .. } | Self::String { .. } | Self::CodePtr { .. } => Vec::new(),
            Self::Print { src, .. } | Self::Stringify { src, .. } | Self::Unbox { src, .. } | Self::TagArrayKind { src, .. } | Self::Panic { msg: src, .. } => vec![*src],
            Self::Format { fmt, args, .. } => {
                let mut v = vec![*fmt];
                v.extend(args.iter().copied());
                v
            }
        }
    }

    pub fn rewrite_values(&mut self, mut map: impl FnMut(ValueId) -> ValueId) {
        match self {
            Self::Const { .. } => {}
            Self::Bin { lhs, rhs, .. } | Self::Cmp { lhs, rhs, .. } | Self::StrEq { lhs, rhs, .. } => {
                *lhs = map(*lhs);
                *rhs = map(*rhs);
            }
            Self::Unary { src, .. } | Self::Cast { src, .. } => *src = map(*src),
            Self::Phi { args, .. } => {
                for (_, v) in args.iter_mut() {
                    *v = map(*v);
                }
            }
            Self::HostInvoke { args, .. } | Self::Call { args, .. } => {
                for v in args.iter_mut() {
                    *v = map(*v);
                }
            }
            Self::CallIndirect { callee, args, .. } => {
                for v in args.iter_mut() {
                    *v = map(*v);
                }
                *callee = map(*callee);
            }
            Self::MatchPayload { scrutinee, .. } => *scrutinee = map(*scrutinee),
            Self::FieldLoad { object, .. } => *object = map(*object),
            Self::FieldStore { src, .. } => *src = map(*src),
            Self::HeapFieldLoad {
                object, name, ..
            } => {
                *object = map(*object);
                if let Some(n) = name {
                    *n = map(*n);
                }
            }
            Self::HeapFieldStore {
                object,
                value,
                name,
                ..
            } => {
                *object = map(*object);
                *value = map(*value);
                if let Some(n) = name {
                    *n = map(*n);
                }
            }
            Self::Index { array, index, .. } => {
                *array = map(*array);
                *index = map(*index);
            }
            Self::StoreIndex {
                array,
                index,
                value,
                ..
            } => {
                *array = map(*array);
                *index = map(*index);
                *value = map(*value);
            }
            Self::ArrayLen { array, .. } => *array = map(*array),
            Self::ResumeCoro { handle, send, .. } => {
                *handle = map(*handle);
                if let Some(s) = send {
                    *s = map(*s);
                }
            }
            Self::Yield { value, .. } => *value = map(*value),
            Self::DoneCoro { handle, .. } => *handle = map(*handle),
            Self::LoadStatic { .. } | Self::HomeLoad { .. } => {}
            Self::StoreStatic { value, .. } | Self::HomeStore { value, .. } | Self::ClearWord { value, .. } | Self::KeepAlive { value, .. } => *value = map(*value),
            Self::ArrayPush { array, value, .. } => {
                *array = map(*array);
                *value = map(*value);
            }
            Self::Alloc { elems, .. } => {
                for v in elems.iter_mut() {
                    *v = map(*v);
                }
            }
            Self::GcBarrier { roots, .. } => {
                for v in roots.iter_mut() {
                    *v = map(*v);
                }
            }
            Self::Deopt { .. } | Self::String { .. } | Self::CodePtr { .. } => {}
            Self::Print { src, .. } | Self::Stringify { src, .. } | Self::Unbox { src, .. } | Self::TagArrayKind { src, .. } | Self::Panic { msg: src, .. } => *src = map(*src),
            Self::Format { fmt, args, .. } => {
                *fmt = map(*fmt);
                for v in args.iter_mut() {
                    *v = map(*v);
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Terminator {
    Jump {
        dest: BlockId,
    },
    /// `cond` is `bool`. `taken` when true.
    Br {
        cond: ValueId,
        taken: BlockId,
        not_taken: BlockId,
    },
    /// Peek-match on a boxed / niche / two-slot enum word (I2 / D3).
    ///
    /// Taken pops the scrutinee and binds `payloads` (declaration order;
    /// last word TOS), including boxed-overlap `JumpIfMatch` arity 0 with
    /// a unary payload. Miss leaves the scrutinee on the stack (same as
    /// bytecode `JumpIfMatch`).
    JumpIfMatch {
        scrutinee: ValueId,
        tag: u32,
        payloads: Vec<ValueId>,
        taken: BlockId,
        not_taken: BlockId,
    },
    /// One-word `Value`/`HeapNiche` (`lo` only) or two-slot (`lo` = payload
    /// or first product word, `hi` = tag / second word on top).
    Return {
        lo: Option<ValueId>,
        hi: Option<ValueId>,
    },
    Unreachable,
}

impl Terminator {
    pub fn succs(&self) -> Vec<BlockId> {
        match *self {
            Self::Jump { dest } => vec![dest],
            Self::Br {
                taken, not_taken, ..
            }
            | Self::JumpIfMatch {
                taken, not_taken, ..
            } => vec![taken, not_taken],
            Self::Return { .. } | Self::Unreachable => Vec::new(),
        }
    }

    pub fn rewrite_values(&mut self, mut map: impl FnMut(ValueId) -> ValueId) {
        match self {
            Self::Br { cond, .. } => *cond = map(*cond),
            Self::JumpIfMatch {
                scrutinee,
                payloads,
                ..
            } => {
                *scrutinee = map(*scrutinee);
                for v in payloads {
                    *v = map(*v);
                }
            }
            Self::Return { lo, hi } => {
                if let Some(v) = lo {
                    *v = map(*v);
                }
                if let Some(v) = hi {
                    *v = map(*v);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f32_const_roundtrips_through_f64_view() {
        let c = MirConst::f32(1.5);
        assert_eq!(c.ty(), MirTy::F32);
        assert_eq!(c.as_f64(), Some(1.5));
        assert_eq!(MirConst::f64(-2.25).as_f64(), Some(-2.25));
        assert_eq!(MirConst::f64(-2.25).ty(), MirTy::F64);
        assert_eq!(MirConst::I32(1).as_f64(), None);
        assert_eq!(MirConst::Bool(true).as_f64(), None);
    }
}
