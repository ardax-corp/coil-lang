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
            _ => return None,
        })
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
        args: Vec<ValueId>,
    },
    /// Dense→dense `CALL` (COI-291). Args are typed SSA; emit `LOAD`s +
    /// one-word `Entry` + `STORE` (same bits as the callee's slots `0..n`).
    Call {
        dest: ValueId,
        target: crate::il::Label,
        args: Vec<ValueId>,
    },
    /// Taken-edge payload of [`Terminator::JumpIfMatch`] (I2). Arity ≤ 1.
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
    /// Store into an unboxed class field slot (ctor / rebind). Escaping
    /// `p.x = …` stays fuse-IL (`local_escape` poisons those).
    FieldStore {
        dest: ValueId,
        src: ValueId,
        base: u32,
        index: u32,
    },
    /// Heap allocation (I5). Dest is [`MirTy::HeapRef`]. Always a GC
    /// safepoint; pair with [`Self::GcBarrier`] so later islands can
    /// attach roots. Dense / LIR emit refuse these bodies.
    Alloc {
        dest: ValueId,
        kind: MirAllocKind,
        elems: Vec<ValueId>,
    },
    /// GC safepoint / write-barrier placeholder (I5). Dest is `HeapRef`
    /// (identity of the first root, or a dummy token). `roots` is empty
    /// until stack maps exist — do not treat this as a precise map.
    GcBarrier {
        dest: ValueId,
        kind: MirGcKind,
        roots: Vec<ValueId>,
    },
}

/// Heap object constructed by [`MirInst::Alloc`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum MirAllocKind {
    Array,
    Tuple,
    /// `InitTyped` (`type_id`, `nfields`). Fields stay fuse-IL `SetField`.
    Object { type_id: u32, nfields: u32 },
    Enum { tag: u32 },
}

impl MirAllocKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Array => "array",
            Self::Tuple => "tuple",
            Self::Object { .. } => "object",
            Self::Enum { .. } => "enum",
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
            | Self::MatchPayload { dest, .. }
            | Self::FieldLoad { dest, .. }
            | Self::FieldStore { dest, .. }
            | Self::Alloc { dest, .. }
            | Self::GcBarrier { dest, .. } => dest,
        }
    }

    /// Allocation or GC placeholder — specialize / native must not cross.
    pub fn is_gc_edge(&self) -> bool {
        matches!(self, Self::Alloc { .. } | Self::GcBarrier { .. })
    }

    pub fn is_phi(&self) -> bool {
        matches!(self, Self::Phi { .. })
    }

    pub fn operands(&self) -> Vec<ValueId> {
        match self {
            Self::Const { .. } => Vec::new(),
            Self::Bin { lhs, rhs, .. } | Self::Cmp { lhs, rhs, .. } => vec![*lhs, *rhs],
            Self::Unary { src, .. } | Self::Cast { src, .. } => vec![*src],
            Self::Phi { args, .. } => args.iter().map(|(_, v)| *v).collect(),
            Self::HostInvoke { args, .. } | Self::Call { args, .. } => args.clone(),
            Self::MatchPayload { scrutinee, .. } => vec![*scrutinee],
            Self::FieldLoad { object, .. } => vec![*object],
            Self::FieldStore { src, .. } => vec![*src],
            Self::Alloc { elems, .. } => elems.clone(),
            Self::GcBarrier { roots, .. } => roots.clone(),
        }
    }

    pub fn rewrite_values(&mut self, mut map: impl FnMut(ValueId) -> ValueId) {
        match self {
            Self::Const { .. } => {}
            Self::Bin { lhs, rhs, .. } | Self::Cmp { lhs, rhs, .. } => {
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
            Self::MatchPayload { scrutinee, .. } => *scrutinee = map(*scrutinee),
            Self::FieldLoad { object, .. } => *object = map(*object),
            Self::FieldStore { src, .. } => *src = map(*src),
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
    /// Peek-match on a niche / two-slot-shaped enum word (I2).
    ///
    /// Taken pops the scrutinee and binds `payloads` (arity ≤ 1). Miss
    /// leaves the scrutinee on the stack (same as bytecode `JumpIfMatch`).
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
