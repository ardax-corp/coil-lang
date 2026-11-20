//! Encoded lane bodies: what a `VecLoop` or `ByteScan` runs per step.
//!
//! A body is a short list of [`LaneWord`]s the runtime's lane code decodes
//! once (`machine/src/lanes.rs`). In MIR and in an archive each word is a
//! `u64` ([`LaneWord::to_bits`]).

/// What one lane word does.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LaneOp {
    #[default]
    VLoad,
    VStore,
    VBin,
    VFma,
    VReduce,
    VLoop,
    VScan,
}

impl LaneOp {
    const ALL: [Self; 7] = [Self::VLoad, Self::VStore, Self::VBin, Self::VFma, Self::VReduce, Self::VLoop, Self::VScan];

    /// The op a word's low byte names.
    pub fn from_u8(code: u8) -> Option<Self> {
        Self::ALL.get(usize::from(code)).copied()
    }
}

/// One word of a lane body: a [`LaneOp`] and `[kind, dest, a, b]` bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LaneWord {
    op: LaneOp,
    operands: u32,
}

impl LaneWord {
    pub fn new(op: LaneOp) -> Self {
        Self { op, operands: 0 }
    }

    pub fn with_abc(mut self, kind: u8, dest: u8, a: u8, b: u8) -> Self {
        self.operands = (u32::from(kind) << 24) | (u32::from(dest) << 16) | (u32::from(a) << 8) | u32::from(b);
        self
    }

    pub fn abc(&self) -> (u8, usize, usize, usize) {
        let o = self.operands;
        ((o >> 24) as u8, ((o >> 16) & 0xFF) as usize, ((o >> 8) & 0xFF) as usize, (o & 0xFF) as usize)
    }

    pub fn op(&self) -> LaneOp {
        self.op
    }

    pub fn to_bits(self) -> u64 {
        u64::from(self.op as u8) | (u64::from(self.operands) << 32)
    }

    /// The word [`Self::to_bits`] made; `None` for one that names no op.
    pub fn from_bits(bits: u64) -> Option<Self> {
        Some(Self { op: LaneOp::from_u8(bits as u8)?, operands: (bits >> 32) as u32 })
    }
}

/// Lane element and operation kinds.
pub mod dense {
    pub const IADD64: u8 = 0;
    pub const ISUB64: u8 = 1;
    pub const IMUL64: u8 = 2;
    pub const IDIV64: u8 = 3;
    pub const IAND64: u8 = 5;
    pub const IOR64: u8 = 6;
    pub const IXOR64: u8 = 7;
    pub const FADD64: u8 = 10;
    pub const FSUB64: u8 = 11;
    pub const FMUL64: u8 = 12;
    pub const FDIV64: u8 = 13;
    pub const CMP_LT: u8 = 0;
    pub const CMP_LE: u8 = 1;
    pub const CMP_GT: u8 = 2;
    pub const CMP_GE: u8 = 3;
    pub const CMP_EQ: u8 = 4;
    pub const CMP_NE: u8 = 5;

    pub const TY_I64: u8 = 1;
    pub const TY_F64: u8 = 3;
}

/// Lane loop shapes and limits.
pub mod simd {
    use super::dense;

    pub const LANES: usize = 8;
    pub const NREGS: usize = 8;

    pub const IADD64: u8 = dense::IADD64;
    pub const ISUB64: u8 = dense::ISUB64;
    pub const IMUL64: u8 = dense::IMUL64;
    pub const IDIV64: u8 = dense::IDIV64;
    pub const FADD64: u8 = dense::FADD64;
    pub const FSUB64: u8 = dense::FSUB64;
    pub const FMUL64: u8 = dense::FMUL64;
    pub const FDIV64: u8 = dense::FDIV64;
    /// Int lane bit ops (archive 6.3); they never trap.
    pub const IAND64: u8 = dense::IAND64;
    pub const IOR64: u8 = dense::IOR64;
    pub const IXOR64: u8 = dense::IXOR64;
    pub const SPLAT_I64: u8 = 32;
    pub const SPLAT_F64: u8 = 33;
    pub const IOTA_I64: u8 = 34;
    pub const IOTA_F64: u8 = 35;
    pub const INEG: u8 = 36;
    pub const FNEG: u8 = 37;
    /// Lane casts (archive 6.11): int lanes to float lanes, and float
    /// lanes to int lanes truncating as `as int` does (NaN is `0`, out of
    /// range saturates). Neither traps.
    pub const I2F: u8 = 38;
    pub const F2I: u8 = 39;

    /// Low byte of `VReduce`: fold the lanes into the scalar. `0` is add,
    /// which is what every existing archive emits.
    pub const REDUCE_ADD: u8 = 0;
    pub const REDUCE_MUL: u8 = 1;
    /// Int bit folds (archive 6.3).
    pub const REDUCE_AND: u8 = 2;
    pub const REDUCE_OR: u8 = 3;
    pub const REDUCE_XOR: u8 = 4;
    /// `REDUCE_TAKE + pred` (archive 6.3, `pred` one of the `CMP_LT` code
    /// ..= `CMP_GE`): lane by lane in order, `acc = x` when `x pred acc`
    /// (a max is `CMP_GT`, a min `CMP_LT`; floats compare as IEEE).
    pub const REDUCE_TAKE: u8 = 8;
    /// `VBin` `kind = ICMP + pred` (archive 6.3, the `CMP_*` code): int lanes
    /// `a pred b` as `1` / `0` int lanes. `FCMP + pred` compares float
    /// lanes (IEEE: a NaN compares unequal and unordered).
    pub const ICMP: u8 = 64;
    pub const FCMP: u8 = 72;
    /// `VBin` select (archive 6.11): `dest = dest != 0 ? a : b` lane by
    /// lane, the mask in `dest` as `0` / `1` int lanes. Never traps.
    pub const SELECT: u8 = 80;

    /// Byte lanes (minor 46, `docs/internals/byte-simd.md`): a vector
    /// register read as [`BYTE_LANES`] `u8` lanes. A lane mask is `0xFF` /
    /// `0x00` per lane.
    pub const BYTE_LANES: usize = 64;
    /// Most body words one `VScan` runs (the length field's low six bits).
    pub const SCAN_MAX_BODY: usize = 63;
    /// `VScan` length-field flag: the loop's test is `iv <= bound`
    /// (`iv >= bound` with [`SCAN_BACKWARD`]).
    pub const SCAN_INCLUSIVE: u8 = 0x80;
    /// `VScan` length-field flag (minor 48): the loop counts down to a
    /// lower bound.
    pub const SCAN_BACKWARD: u8 = 0x40;
    /// `VLoop` length-field flag (archive 6.2): `dest` holds the block
    /// position `p` while the body runs (a splat of it is the index).
    pub const LOOP_POS: u8 = 0x80;
    /// `VLoop` length-field bits that hold the body length.
    pub const LOOP_LEN: u8 = 0x7F;
    /// `VStore` type flag (archive 6.11, `VLoop` bodies): append the 8
    /// lanes to the array in lane order instead of storing them at an
    /// index (the index slot is unused).
    pub const STORE_PUSH: u8 = 0x80;
    /// `VLoad` type: 64 bytes of a `string` or a byte array from `idx`.
    pub const TY_U8: u8 = 8;
    /// `VBin` mask logic on byte lanes: `a & b`, `a | b`, `a & !b`, `!a`.
    pub const M_AND: u8 = 40;
    pub const M_OR: u8 = 41;
    pub const M_ANDN: u8 = 42;
    pub const M_NOT: u8 = 43;
    /// `VBin` `kind = B_CMP + pred` (the `CMP_*` code): unsigned lane `a pred
    /// b`, both vector registers.
    pub const B_CMP: u8 = 48;
    /// `VBin` `kind = BS_CMP + pred`: lane `b` (a vector register) `pred` the
    /// `i64` in frame slot `a`. A scalar outside `0..=255` compares as that
    /// integer (`x == 300` is false in every lane).
    pub const BS_CMP: u8 = 56;

    /// Whether `kind` is a byte-lane `VBin` kind.
    #[inline]
    pub const fn is_byte_kind(kind: u8) -> bool {
        kind >= M_AND && kind < BS_CMP + 6
    }

    /// Whether a `VBin` of `kind` reads only its `a` register.
    #[inline]
    pub const fn is_unary_kind(kind: u8) -> bool {
        matches!(kind, INEG | FNEG | I2F | F2I)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_word_survives_its_bits() {
        let word = LaneWord::new(LaneOp::VBin).with_abc(1, 2, 3, 4);
        assert_eq!(LaneWord::from_bits(word.to_bits()), Some(word));
    }

    #[test]
    fn a_word_naming_no_op_does_not_decode() {
        assert_eq!(LaneWord::from_bits(255), None);
    }
}
