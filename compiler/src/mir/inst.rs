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
    I64(i64),
    /// IEEE bits (Eq/Hash-friendly).
    F64(u64),
    Bool(bool),
}

impl MirConst {
    pub fn ty(self) -> MirTy {
        match self {
            Self::I64(_) => MirTy::I64,
            Self::F64(_) => MirTy::F64,
            Self::Bool(_) => MirTy::Bool,
        }
    }

    pub fn f64(v: f64) -> Self {
        Self::F64(v.to_bits())
    }

    #[allow(dead_code)]
    pub fn as_f64(self) -> Option<f64> {
        match self {
            Self::F64(b) => Some(f64::from_bits(b)),
            _ => None,
        }
    }
}

impl Eq for MirConst {}

impl std::hash::Hash for MirConst {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match *self {
            Self::I64(v) => v.hash(state),
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

    #[cfg(test)]
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

    #[cfg(test)]
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

/// A lane operand of a [`MirInst::ByteScan`] predicate.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum LaneVal {
    /// Byte `k` of stream `s` in lane `k`.
    Stream(u8),
    /// The same `i64` (an operand of the instruction) in every lane.
    Scalar(u8),
}

/// A [`MirInst::ByteScan`] predicate over 64 byte lanes.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum LaneExpr {
    Const(bool),
    /// Integer compare of two lane values (bytes are `0..=255`).
    Cmp {
        op: MirCmpOp,
        lhs: LaneVal,
        rhs: LaneVal,
    },
    And(Box<LaneExpr>, Box<LaneExpr>),
    Or(Box<LaneExpr>, Box<LaneExpr>),
    Not(Box<LaneExpr>),
}

impl LaneExpr {
    /// `a & b`, folding constants, `a & a`, `a & !a`,
    /// `a & (!a | b)` (what a branch chain's masks look like), and on
    /// `&` chains `x & (x & y)` and a term against its negation.
    pub fn and(a: Self, b: Self) -> Self {
        match (a, b) {
            (Self::Const(false), _) | (_, Self::Const(false)) => Self::Const(false),
            (Self::Const(true), x) | (x, Self::Const(true)) => x,
            (a, b) if a == b => a,
            (a, b) if a.is_not_of(&b) => Self::Const(false),
            (a, Self::Or(x, y)) if x.is_not_of(&a) => Self::and(a, *y),
            (Self::Or(x, y), b) if x.is_not_of(&b) => Self::and(*y, b),
            (a, b) => {
                let (ca, cb) = (a.conjuncts(), b.conjuncts());
                if ca.iter().any(|x| cb.iter().any(|y| x.is_not_of(y))) {
                    return Self::Const(false);
                }
                if cb.iter().all(|y| ca.contains(y)) {
                    return a;
                }
                if ca.iter().all(|x| cb.contains(x)) {
                    return b;
                }
                Self::And(Box::new(a), Box::new(b))
            }
        }
    }

    /// The terms of an `&` chain (`self` alone when it is not one).
    fn conjuncts(&self) -> Vec<&Self> {
        match self {
            Self::And(x, y) => {
                let mut out = x.conjuncts();
                out.extend(y.conjuncts());
                out
            }
            e => vec![e],
        }
    }

    /// `terms` joined by `&`; `true` when empty.
    fn all(terms: &[&Self]) -> Self {
        terms.iter().fold(Self::Const(true), |acc, t| Self::and(acc, (*t).clone()))
    }

    /// `a | b`, folding constants, `a | a`, `a | !a`, `a | (!a & b)`,
    /// and the terms two `&` chains share: `(x & a) | (x & b)` is
    /// `x & (a | b)`, so the two ways out of a branch join back to `x`.
    pub fn or(a: Self, b: Self) -> Self {
        match (a, b) {
            (Self::Const(true), _) | (_, Self::Const(true)) => Self::Const(true),
            (Self::Const(false), x) | (x, Self::Const(false)) => x,
            (a, b) if a == b => a,
            (a, b) if a.is_not_of(&b) => Self::Const(true),
            (a, Self::And(x, y)) if x.is_not_of(&a) => Self::or(a, *y),
            (Self::And(x, y), b) if x.is_not_of(&b) => Self::or(*y, b),
            (a, b) => {
                let (ca, cb) = (a.conjuncts(), b.conjuncts());
                let shared: Vec<&Self> = ca.iter().copied().filter(|x| cb.contains(x)).collect();
                if shared.is_empty() {
                    return Self::Or(Box::new(a), Box::new(b));
                }
                let rest = |c: &[&Self]| Self::all(&c.iter().copied().filter(|x| !shared.contains(x)).collect::<Vec<_>>());
                Self::and(Self::all(&shared), Self::or(rest(&ca), rest(&cb)))
            }
        }
    }

    /// Whether one of `self` / `other` is the negation of the other
    /// (`!a`, or a compare with the opposite predicate).
    fn is_not_of(&self, other: &Self) -> bool {
        Self::not(self.clone()) == *other || Self::not(other.clone()) == *self
    }

    pub fn not(a: Self) -> Self {
        match a {
            Self::Const(b) => Self::Const(!b),
            Self::Not(x) => *x,
            Self::Cmp { op, lhs, rhs } => {
                let op = match op {
                    MirCmpOp::Lt => MirCmpOp::Ge,
                    MirCmpOp::Le => MirCmpOp::Gt,
                    MirCmpOp::Gt => MirCmpOp::Le,
                    MirCmpOp::Ge => MirCmpOp::Lt,
                    MirCmpOp::Eq => MirCmpOp::Ne,
                    MirCmpOp::Ne => MirCmpOp::Eq,
                };
                Self::Cmp { op, lhs, rhs }
            }
            // De Morgan when it costs no nodes: `!(a | b)` with compares
            // on both sides is `!a & !b`, the shape the VM's fast scans
            // match (a threaded `&&` chain's mask, #1976).
            Self::And(x, y) => {
                let size = 1 + x.size() + y.size();
                let pushed = Self::or(Self::not((*x).clone()), Self::not((*y).clone()));
                if pushed.size() <= size {
                    pushed
                } else {
                    Self::Not(Box::new(Self::And(x, y)))
                }
            }
            Self::Or(x, y) => {
                let size = 1 + x.size() + y.size();
                let pushed = Self::and(Self::not((*x).clone()), Self::not((*y).clone()));
                if pushed.size() <= size {
                    pushed
                } else {
                    Self::Not(Box::new(Self::Or(x, y)))
                }
            }
        }
    }

    /// Nodes in the tree.
    pub fn size(&self) -> usize {
        match self {
            Self::Const(_) | Self::Cmp { .. } => 1,
            Self::And(a, b) | Self::Or(a, b) => 1 + a.size() + b.size(),
            Self::Not(a) => 1 + a.size(),
        }
    }
}

impl std::fmt::Display for LaneVal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stream(s) => write!(f, "s{s}"),
            Self::Scalar(k) => write!(f, "k{k}"),
        }
    }
}

impl std::fmt::Display for LaneExpr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Const(b) => write!(f, "{b}"),
            Self::Cmp { op, lhs, rhs } => write!(f, "({lhs} {} {rhs})", op.as_str(false)),
            Self::And(a, b) => write!(f, "({a} & {b})"),
            Self::Or(a, b) => write!(f, "({a} | {b})"),
            Self::Not(a) => write!(f, "!{a}"),
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum MirCastKind {
    /// `CastIntToFloat` — signed int to IEEE float of matching width or widen.
    IntToFloat,
    /// `CastFloatToInt` — `f64` → `i64`, truncating toward zero.
    FloatToInt,
    /// The same word retyped: a heap pointer or the `i64` zero (`None` /
    /// `Ok(())`) as a `NicheOpt` / `NicheRes` word, such a word known to
    /// hold a pointer as `HeapRef`, or a heap, niche, `bool` or `f64` word
    /// moved through an opaque `i64` pair word and back (its bits as they
    /// are).
    Word,
}

impl MirCastKind {
    /// Whether a cast of this kind from `from` to `to` is legal.
    pub fn allows(self, from: MirTy, to: MirTy) -> bool {
        match self {
            Self::IntToFloat => from.is_int() && to.is_float(),
            Self::FloatToInt => from == MirTy::F64 && to == MirTy::I64,
            Self::Word => {
                let niche = |t: MirTy| matches!(t, MirTy::NicheOpt | MirTy::NicheRes);
                (niche(to) && matches!(from, MirTy::I64 | MirTy::HeapRef))
                    || (niche(from) && matches!(to, MirTy::HeapRef | MirTy::I64))
                    || matches!((from, to), (MirTy::I64, MirTy::HeapRef) | (MirTy::HeapRef, MirTy::I64))
                    // A `bool` word is 0 or 1: as an `i64` it is the same bits.
                    || matches!((from, to), (MirTy::Bool, MirTy::I64) | (MirTy::I64, MirTy::Bool))
                    // A `float` payload rides an `i64` pair word beside an
                    // `int` one: the same 64 bits, moved and not converted.
                    || matches!((from, to), (MirTy::F64, MirTy::I64) | (MirTy::I64, MirTy::F64))
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
    /// An FFI opcode (`extern` calls and `dload` / `declare` / `invoke`):
    /// `args` are its stack operands in push order, `operand` the opcode's
    /// own; `dest` is the boxed `Result` the VM pushes. A call that may run
    /// a callback: an effect barrier, as an impure HostInvoke; dense refuses.
    Ffi {
        dest: ValueId,
        op: MirFfiOp,
        operand: u32,
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
    /// `PolyFn` heap word, or an `i64` code pointer read from an instance
    /// dictionary) with `args`, pushed in order under it. Dest is
    /// the one word it returns. Opaque: any effect, a GC safepoint in the
    /// callee; never CSE'd or hoisted.
    CallIndirect {
        dest: ValueId,
        callee: ValueId,
        args: Vec<ValueId>,
        /// How many of `args` (the last ones) are instance dictionaries a
        /// `PolyFn` callee takes after its arguments.
        dicts: u32,
    },
    /// Taken-edge / last-arm payload of [`Terminator::JumpIfMatch`] or
    /// `Unpack` (I2 / D3). `index` is the declaration-order payload word.
    MatchPayload {
        dest: ValueId,
        scrutinee: ValueId,
        index: u32,
    },
    /// Heap `LoadField` of slot `index` on an escaping object (D1 maps).
    /// `object` is [`MirTy::HeapRef`]. Dest is the field word (not SROA).
    HeapFieldLoad {
        dest: ValueId,
        object: ValueId,
        index: u32,
    },
    /// Heap `SetField` of slot `index`. Dest is the stored value (VM
    /// leaves it TOS).
    HeapFieldStore {
        dest: ValueId,
        object: ValueId,
        value: ValueId,
        index: u32,
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
    /// S2c may emit across this edge when every allocation has its barrier.
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
    /// native tier must leave or pause at. Only tests build one today.
    #[cfg_attr(not(test), allow(dead_code))]
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
    /// Byte-lane scan (`byte_scan`, archive minor 46). From `p = from` in
    /// steps of 64, while all 64 lanes `p + k` pass the loop's test
    /// (`p + k < bound`, or `<=` when `inclusive`) and every stream holds
    /// them: the first `p + k` (`k` in `0..64`) where `expr` holds at
    /// lane `k`; else the `p` it stopped at. Stream `s` reads byte
    /// `p + streams[s].1 + k` of the string or byte array `streams[s].0`,
    /// and `LaneVal::Scalar(j)` is `scalars[j]`. Pure; reads immutable
    /// bytes only. When `backward` (archive minor 48) the lanes are
    /// `p - 63 + k` and must pass `> bound` (`>=` when `inclusive`), the
    /// match is the highest lane, and `p` steps down by 64.
    ByteScan {
        dest: ValueId,
        from: ValueId,
        bound: ValueId,
        inclusive: bool,
        backward: bool,
        streams: Vec<(ValueId, ValueId)>,
        scalars: Vec<ValueId>,
        expr: LaneExpr,
    },
}

/// The FFI opcode a [`MirInst::Ffi`] reconstructs.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum MirFfiOp {
    /// `FfiLoad`: the library name.
    Load,
    /// `DeclareFFI`: library, name, signature tag tuple, return tag.
    Declare,
    /// `FfiInvoke`: library, function id, argument tuple, and a variadic
    /// call's tag tuple.
    Invoke,
}

impl MirFfiOp {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Load => "load",
            Self::Declare => "declare",
            Self::Invoke => "invoke",
        }
    }

    pub fn instruction(self) -> common::Instruction {
        match self {
            Self::Load => common::Instruction::FfiLoad,
            Self::Declare => common::Instruction::DeclareFFI,
            Self::Invoke => common::Instruction::FfiInvoke,
        }
    }
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
    /// `MakePolyFnCapture`: a generic function's value carrying its
    /// instance dictionaries, the elems (a `0` word for evidence the call
    /// fills in); `CodePtr` of `target` is pushed after them. `params` is
    /// its declared parameter count (0: unrecorded), so a call given more
    /// hands the rest to its result (#1589).
    PolyFnCapture {
        target: crate::il::Label,
        params: u32,
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
            Self::Coro { .. } => "coro",
            Self::Boxed { .. } => "boxed",
            Self::Fn { .. } => "fn",
            Self::PolyFn { .. } => "polyfn",
            Self::PolyFnCapture { .. } => "polyfncap",
        }
    }
}

/// Placeholder GC coordination on an alloc / future call edge (I5).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum MirGcKind {
    Safepoint,
}

impl MirGcKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Safepoint => "safepoint",
        }
    }

    #[cfg(test)]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "safepoint" => Self::Safepoint,
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
            | Self::Ffi { dest, .. }
            | Self::Call { dest, .. }
            | Self::CallIndirect { dest, .. }
            | Self::MatchPayload { dest, .. }
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
            | Self::StrEq { dest, .. }
            | Self::ByteScan { dest, .. } => dest,
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
            Self::HostInvoke { args, .. } | Self::Ffi { args, .. } | Self::Call { args, .. } => args.clone(),
            Self::CallIndirect { callee, args, .. } => args.iter().copied().chain([*callee]).collect(),
            Self::MatchPayload { scrutinee, .. } => vec![*scrutinee],
            Self::HeapFieldLoad { object, .. } => vec![*object],
            Self::HeapFieldStore { object, value, .. } => vec![*object, *value],
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
            Self::Print { src, .. } | Self::Stringify { src, .. } | Self::Unbox { src, .. } | Self::Panic { msg: src, .. } => vec![*src],
            Self::Format { fmt, args, .. } => {
                let mut v = vec![*fmt];
                v.extend(args.iter().copied());
                v
            }
            Self::ByteScan { from, bound, streams, scalars, .. } => [*from, *bound]
                .into_iter()
                .chain(streams.iter().flat_map(|&(a, i)| [a, i]))
                .chain(scalars.iter().copied())
                .collect(),
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
            Self::HostInvoke { args, .. } | Self::Ffi { args, .. } | Self::Call { args, .. } => {
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
            Self::HeapFieldLoad { object, .. } => *object = map(*object),
            Self::HeapFieldStore { object, value, .. } => {
                *object = map(*object);
                *value = map(*value);
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
            Self::Print { src, .. } | Self::Stringify { src, .. } | Self::Unbox { src, .. } | Self::Panic { msg: src, .. } => *src = map(*src),
            Self::Format { fmt, args, .. } => {
                *fmt = map(*fmt);
                for v in args.iter_mut() {
                    *v = map(*v);
                }
            }
            Self::ByteScan { from, bound, streams, scalars, .. } => {
                *from = map(*from);
                *bound = map(*bound);
                for (a, i) in streams.iter_mut() {
                    *a = map(*a);
                    *i = map(*i);
                }
                for v in scalars.iter_mut() {
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
    /// Whether `block` is a successor (as [`Self::succs`], without
    /// allocating).
    pub fn targets(&self, block: BlockId) -> bool {
        match *self {
            Self::Jump { dest } => dest == block,
            Self::Br { taken, not_taken, .. } | Self::JumpIfMatch { taken, not_taken, .. } => {
                taken == block || not_taken == block
            }
            Self::Return { .. } | Self::Unreachable => false,
        }
    }

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
    fn f64_const_roundtrips_through_f64_view() {
        assert_eq!(MirConst::f64(-2.25).as_f64(), Some(-2.25));
        assert_eq!(MirConst::f64(-2.25).ty(), MirTy::F64);
        assert_eq!(MirConst::I64(1).as_f64(), None);
        assert_eq!(MirConst::Bool(true).as_f64(), None);
    }

    fn ne(k: u8) -> LaneExpr {
        LaneExpr::Cmp { op: MirCmpOp::Ne, lhs: LaneVal::Stream(0), rhs: LaneVal::Scalar(k) }
    }

    /// A four-way `c != a && c != b && ...` chain, as the walk builds it
    /// from its branches and `bool` φs, folds back to the plain chain.
    #[test]
    fn lane_expr_factors_shared_conjuncts() {
        let x = LaneExpr::and(ne(0), ne(1));
        // `(x & !k2) | (x & k2)` is `x`.
        assert_eq!(LaneExpr::or(LaneExpr::and(x.clone(), LaneExpr::not(ne(2))), LaneExpr::and(x.clone(), ne(2))), x);
        // `x & (x & k2)` is `x & k2`; a term against its negation is false.
        let x2 = LaneExpr::and(x.clone(), ne(2));
        assert_eq!(LaneExpr::and(x.clone(), x2.clone()), x2);
        assert_eq!(LaneExpr::and(x2, LaneExpr::and(ne(3), LaneExpr::not(ne(1)))), LaneExpr::Const(false));
        // Nothing shared: left as it is.
        assert_eq!(LaneExpr::or(ne(0), ne(1)).size(), 3);
    }
}
