//! Operand-order canonicalization for stack IL.
//!
//! Rewrites Known-SP windows into preferred forms so fuse-select, algebraic
//! peeps, and GVN/CSE match more often:
//! - `Const; Load; op` → `Load; Const; op'` (const on RHS)
//! - `ConstPool; Load; int-op` → demote pool to inline `Const` when safe, then swap
//! - `Load a; Load b; op` with `a > b` → swapped loads (+ cmp polarity flip)
//!
//! Refuses: Unknown SP, float ops, residual `Byte`, and non-commutative ops
//! (`SUB`/`DIV`/`MOD`/`SHL`/`SHR`/`Pow`). No float reassoc.

use std::cell::RefCell;

use common::{Byte, Instruction, Value};

use super::op::IlOp;
use super::sp;

thread_local! {
    static LAST_STATS: RefCell<CanonStats> = const { RefCell::new(CanonStats::new()) };
}

/// Counters from the most recent compile's accumulated canon runs on this thread.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CanonStats {
    /// `Const; Load; op` → `Load; Const; op'` rewrites (includes post-demote).
    pub const_load_swaps: u32,
    /// High-then-low `Load; Load; op` slot-order swaps.
    pub load_load_swaps: u32,
    /// Ordered-cmp polarity flips (`LE`↔`GT`, `LEQ`↔`GEQ`).
    pub cmp_flips: u32,
    /// Int `ConstPool` demoted to inline `Const` before a swap.
    pub const_pool_demotes: u32,
    /// Windows that matched a rewrite shape but had Unknown SP-in.
    pub refused_unknown_sp: u32,
}

impl CanonStats {
    const fn new() -> Self {
        Self {
            const_load_swaps: 0,
            load_load_swaps: 0,
            cmp_flips: 0,
            const_pool_demotes: 0,
            refused_unknown_sp: 0,
        }
    }
}

/// Stats from the last compile's accumulated canon runs on this thread.
pub fn last_canon_stats() -> CanonStats {
    LAST_STATS.with(|c| *c.borrow())
}

/// Clear accumulated canon counters (call at compile / lower start).
pub fn reset_canon_stats() {
    LAST_STATS.with(|c| *c.borrow_mut() = CanonStats::new());
}

fn acc(f: impl FnOnce(&mut CanonStats)) {
    LAST_STATS.with(|c| f(&mut c.borrow_mut()));
}

/// Normalize operand order in place when SP-in is Known for the window.
///
/// `pool` supplies `ConstPool` payloads for int demotion; pass an empty slice
/// to disable demotion.
pub fn canonicalize_operand_order(ops: &mut Vec<IlOp>, pool: &[u64]) {
    if ops.len() < 3 {
        return;
    }
    let info = sp::analyze(ops);
    let mut out = Vec::with_capacity(ops.len());
    let mut i = 0;
    while i < ops.len() {
        if i + 2 < ops.len() {
            let known = info.sp_before(i).is_known()
                && info.sp_before(i + 1).is_known()
                && info.sp_before(i + 2).is_known();
            if !known {
                if matches_rewrite_shape(&ops[i], &ops[i + 1], &ops[i + 2], pool) {
                    acc(|s| s.refused_unknown_sp = s.refused_unknown_sp.saturating_add(1));
                }
                out.push(ops[i].clone());
                i += 1;
                continue;
            }
            if let Some((rewritten, demoted, flipped)) =
                try_const_or_pool_load_bin(&ops[i], &ops[i + 1], &ops[i + 2], pool)
            {
                acc(|s| {
                    s.const_load_swaps = s.const_load_swaps.saturating_add(1);
                    if demoted {
                        s.const_pool_demotes = s.const_pool_demotes.saturating_add(1);
                    }
                    if flipped {
                        s.cmp_flips = s.cmp_flips.saturating_add(1);
                    }
                });
                out.extend(rewritten);
                i += 3;
                continue;
            }
            if let Some((rewritten, flipped)) =
                try_load_load_bin(&ops[i], &ops[i + 1], &ops[i + 2])
            {
                acc(|s| {
                    s.load_load_swaps = s.load_load_swaps.saturating_add(1);
                    if flipped {
                        s.cmp_flips = s.cmp_flips.saturating_add(1);
                    }
                });
                out.extend(rewritten);
                i += 3;
                continue;
            }
        }
        out.push(ops[i].clone());
        i += 1;
    }
    *ops = out;
}

fn matches_rewrite_shape(a: &IlOp, b: &IlOp, c: &IlOp, pool: &[u64]) -> bool {
    try_const_or_pool_load_bin(a, b, c, pool).is_some() || try_load_load_bin(a, b, c).is_some()
}

fn is_commute_keep(op: Instruction) -> bool {
    matches!(
        op,
        Instruction::ADD
            | Instruction::MUL
            | Instruction::EQ
            | Instruction::NEQ
            | Instruction::AND
            | Instruction::OR
            | Instruction::BITAND
            | Instruction::BITOR
            | Instruction::XOR
    )
}

fn flip_ordered_cmp(op: Instruction) -> Option<Instruction> {
    Some(match op {
        Instruction::LE => Instruction::GT,
        Instruction::GT => Instruction::LE,
        Instruction::LEQ => Instruction::GEQ,
        Instruction::GEQ => Instruction::LEQ,
        _ => return None,
    })
}

fn swap_binop(op: Instruction) -> Option<Instruction> {
    if is_commute_keep(op) {
        Some(op)
    } else {
        flip_ordered_cmp(op)
    }
}

fn is_int_swap_op(op: Instruction) -> bool {
    swap_binop(op).is_some()
}

/// Pool int → inline `Const` when non-negative and free of `POOL_FLAG` bit 31.
fn demote_pool_int(pool: &[u64], idx: u32, op: Instruction) -> Option<i32> {
    if !is_int_swap_op(op) {
        return None;
    }
    let bits = *pool.get(idx as usize)?;
    let n = Value::from(bits).as_int();
    if !(0..=i32::MAX as i64).contains(&n) {
        return None;
    }
    let imm = n as i32;
    if (imm as u32) & Byte::POOL_FLAG != 0 {
        return None;
    }
    Some(imm)
}

/// `Const`/`ConstPool`; `Load`; `op` → `Load; Const; op'` when swappable.
fn try_const_or_pool_load_bin(
    a: &IlOp,
    b: &IlOp,
    c: &IlOp,
    pool: &[u64],
) -> Option<([IlOp; 3], bool, bool)> {
    let IlOp::Load { slot, .. } = b else {
        return None;
    };
    let IlOp::Bin { op, .. } = c else {
        return None;
    };
    let op2 = swap_binop(*op)?;
    let flipped = op2 != *op;
    let (imm, loc, demoted) = match a {
        IlOp::Const { imm, loc } => (*imm, *loc, false),
        IlOp::ConstPool { idx, loc } => {
            let imm = demote_pool_int(pool, *idx, *op)?;
            (imm, *loc, true)
        }
        _ => return None,
    };
    Some((
        [
            IlOp::Load {
                slot: *slot,
                loc,
            },
            IlOp::Const { imm, loc },
            IlOp::Bin {
                op: op2,
                loc,
            },
        ],
        demoted,
        flipped,
    ))
}

/// `Load a; Load b; op` with `a > b` → swapped loads (+ flipped cmp).
fn try_load_load_bin(a: &IlOp, b: &IlOp, c: &IlOp) -> Option<([IlOp; 3], bool)> {
    let (IlOp::Load { slot: sa, loc }, IlOp::Load { slot: sb, .. }, IlOp::Bin { op, .. }) = (a, b, c)
    else {
        return None;
    };
    if *sa <= *sb {
        return None;
    }
    let op2 = swap_binop(*op)?;
    let flipped = op2 != *op;
    Some((
        [
            IlOp::Load {
                slot: *sb,
                loc: *loc,
            },
            IlOp::Load {
                slot: *sa,
                loc: *loc,
            },
            IlOp::Bin {
                op: op2,
                loc: *loc,
            },
        ],
        flipped,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::DebugLoc;

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

    #[test]
    fn const_load_add_swaps_to_load_const_add() {
        reset_canon_stats();
        let mut ops = vec![
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: loc(),
            },
        ];
        canonicalize_operand_order(&mut ops, &[]);
        assert!(matches!(ops[0], IlOp::Load { slot: 0, .. }));
        assert!(matches!(ops[1], IlOp::Const { imm: 1, .. }));
        assert!(matches!(ops[2], IlOp::Bin { op: Instruction::ADD, .. }));
        let s = last_canon_stats();
        assert_eq!(s.const_load_swaps, 1);
        assert_eq!(s.cmp_flips, 0);
    }

    #[test]
    fn const_load_mul_swaps() {
        let mut ops = vec![
            IlOp::Const { imm: 3, loc: loc() },
            IlOp::Load {
                slot: 2,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::MUL,
                loc: loc(),
            },
        ];
        canonicalize_operand_order(&mut ops, &[]);
        assert!(matches!(ops[0], IlOp::Load { slot: 2, .. }));
        assert!(matches!(ops[1], IlOp::Const { imm: 3, .. }));
        assert!(matches!(ops[2], IlOp::Bin { op: Instruction::MUL, .. }));
    }

    #[test]
    fn const_load_eq_keeps_eq() {
        let mut ops = vec![
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::EQ,
                loc: loc(),
            },
        ];
        canonicalize_operand_order(&mut ops, &[]);
        assert!(matches!(ops[0], IlOp::Load { slot: 1, .. }));
        assert!(matches!(ops[1], IlOp::Const { imm: 0, .. }));
        assert!(matches!(ops[2], IlOp::Bin { op: Instruction::EQ, .. }));
    }

    #[test]
    fn const_load_le_becomes_load_const_gt() {
        reset_canon_stats();
        let mut ops = vec![
            IlOp::Const { imm: 5, loc: loc() },
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::LE,
                loc: loc(),
            },
        ];
        canonicalize_operand_order(&mut ops, &[]);
        assert!(matches!(ops[0], IlOp::Load { slot: 0, .. }));
        assert!(matches!(ops[1], IlOp::Const { imm: 5, .. }));
        assert!(matches!(ops[2], IlOp::Bin { op: Instruction::GT, .. }));
        assert_eq!(last_canon_stats().cmp_flips, 1);
    }

    #[test]
    fn const_load_leq_becomes_geq() {
        let mut ops = vec![
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::LEQ,
                loc: loc(),
            },
        ];
        canonicalize_operand_order(&mut ops, &[]);
        assert!(matches!(ops[2], IlOp::Bin { op: Instruction::GEQ, .. }));
    }

    #[test]
    fn const_load_gt_becomes_le() {
        let mut ops = vec![
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::GT,
                loc: loc(),
            },
        ];
        canonicalize_operand_order(&mut ops, &[]);
        assert!(matches!(ops[2], IlOp::Bin { op: Instruction::LE, .. }));
    }

    #[test]
    fn const_load_geq_becomes_leq() {
        let mut ops = vec![
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::GEQ,
                loc: loc(),
            },
        ];
        canonicalize_operand_order(&mut ops, &[]);
        assert!(matches!(ops[2], IlOp::Bin { op: Instruction::LEQ, .. }));
    }

    #[test]
    fn load_const_add_unchanged() {
        let mut ops = vec![
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: loc(),
            },
        ];
        let before = ops.clone();
        canonicalize_operand_order(&mut ops, &[]);
        assert!(ops == before);
    }

    #[test]
    fn const_load_sub_refused() {
        let mut ops = vec![
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::SUB,
                loc: loc(),
            },
        ];
        let before = ops.clone();
        canonicalize_operand_order(&mut ops, &[]);
        assert!(ops == before);
    }

    #[test]
    fn unknown_sp_refused() {
        reset_canon_stats();
        let mut ops = vec![
            IlOp::byte(common::Byte::new(Instruction::FfiInvoke)),
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: loc(),
            },
        ];
        let before = ops.clone();
        canonicalize_operand_order(&mut ops, &[]);
        assert!(ops == before);
        assert!(last_canon_stats().refused_unknown_sp >= 1);
    }

    #[test]
    fn load_high_load_low_add_swaps_slots() {
        reset_canon_stats();
        let mut ops = vec![
            IlOp::Load {
                slot: 3,
                loc: loc(),
            },
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: loc(),
            },
        ];
        canonicalize_operand_order(&mut ops, &[]);
        assert!(matches!(ops[0], IlOp::Load { slot: 1, .. }));
        assert!(matches!(ops[1], IlOp::Load { slot: 3, .. }));
        assert!(matches!(ops[2], IlOp::Bin { op: Instruction::ADD, .. }));
        assert_eq!(last_canon_stats().load_load_swaps, 1);
    }

    #[test]
    fn load_low_load_high_add_unchanged() {
        let mut ops = vec![
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::Load {
                slot: 3,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: loc(),
            },
        ];
        let before = ops.clone();
        canonicalize_operand_order(&mut ops, &[]);
        assert!(ops == before);
    }

    #[test]
    fn load_high_load_low_le_becomes_gt() {
        let mut ops = vec![
            IlOp::Load {
                slot: 4,
                loc: loc(),
            },
            IlOp::Load {
                slot: 2,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::LE,
                loc: loc(),
            },
        ];
        canonicalize_operand_order(&mut ops, &[]);
        assert!(matches!(ops[0], IlOp::Load { slot: 2, .. }));
        assert!(matches!(ops[1], IlOp::Load { slot: 4, .. }));
        assert!(matches!(ops[2], IlOp::Bin { op: Instruction::GT, .. }));
    }

    #[test]
    fn load_high_load_low_sub_refused() {
        let mut ops = vec![
            IlOp::Load {
                slot: 5,
                loc: loc(),
            },
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::SUB,
                loc: loc(),
            },
        ];
        let before = ops.clone();
        canonicalize_operand_order(&mut ops, &[]);
        assert!(ops == before);
    }

    /// Preferred shape after Rewrite A — what `BinSlotImm` fuse expects.
    #[test]
    fn preferred_load_const_add_shape_after_canon() {
        let mut ops = vec![
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        canonicalize_operand_order(&mut ops, &[]);
        assert!(matches!(ops[0], IlOp::Load { slot: 0, .. }));
        assert!(matches!(ops[1], IlOp::Const { imm: 1, .. }));
        assert!(matches!(ops[2], IlOp::Bin { op: Instruction::ADD, .. }));
        assert!(matches!(ops[3], IlOp::Return { .. }));
    }

    #[test]
    fn const_pool_load_add_demotes_and_swaps() {
        reset_canon_stats();
        let pool = vec![Value::from(7_i64).raw() as u64];
        let mut ops = vec![
            IlOp::ConstPool {
                idx: 0,
                loc: loc(),
            },
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: loc(),
            },
        ];
        canonicalize_operand_order(&mut ops, &pool);
        assert!(matches!(ops[0], IlOp::Load { slot: 0, .. }));
        assert!(matches!(ops[1], IlOp::Const { imm: 7, .. }));
        assert!(matches!(ops[2], IlOp::Bin { op: Instruction::ADD, .. }));
        let s = last_canon_stats();
        assert_eq!(s.const_pool_demotes, 1);
        assert_eq!(s.const_load_swaps, 1);
    }

    #[test]
    fn const_pool_float_addf_refused() {
        let pool = vec![1.0_f64.to_bits()];
        let mut ops = vec![
            IlOp::ConstPool {
                idx: 0,
                loc: loc(),
            },
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::ADDF,
                loc: loc(),
            },
        ];
        let before = ops.clone();
        canonicalize_operand_order(&mut ops, &pool);
        assert!(ops == before);
    }

    #[test]
    fn const_load_div_and_pow_refused() {
        for op in [Instruction::DIV, Instruction::Pow, Instruction::SHL] {
            let mut ops = vec![
                IlOp::Const { imm: 2, loc: loc() },
                IlOp::Load {
                    slot: 0,
                    loc: loc(),
                },
                IlOp::Bin { op, loc: loc() },
            ];
            let before = ops.clone();
            canonicalize_operand_order(&mut ops, &[]);
            assert!(ops == before);
        }
    }

    #[test]
    fn const_load_addf_refused() {
        let mut ops = vec![
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::ADDF,
                loc: loc(),
            },
        ];
        let before = ops.clone();
        canonicalize_operand_order(&mut ops, &[]);
        assert!(ops == before);
    }

    #[test]
    fn load_high_load_low_bitand_keeps_op() {
        let mut ops = vec![
            IlOp::Load {
                slot: 5,
                loc: loc(),
            },
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::BITAND,
                loc: loc(),
            },
        ];
        canonicalize_operand_order(&mut ops, &[]);
        assert!(matches!(ops[0], IlOp::Load { slot: 1, .. }));
        assert!(matches!(ops[1], IlOp::Load { slot: 5, .. }));
        assert!(matches!(
            ops[2],
            IlOp::Bin {
                op: Instruction::BITAND,
                ..
            }
        ));
    }

    #[test]
    fn optimize_with_canon_disabled_keeps_const_load() {
        use super::super::opt::{OptimizeOptions, optimize};
        let mut ops = vec![
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        optimize(
            &mut ops,
            &OptimizeOptions {
                canon: false,
                cast_spill: false,
                algebraic: false,
                ..OptimizeOptions::default()
            },
            &mut Vec::new(),
        );
        assert!(matches!(ops[0], IlOp::Const { imm: 1, .. }));
    }

    #[test]
    fn successive_windows_both_canonicalize() {
        let mut ops = vec![
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: loc(),
            },
            IlOp::Const { imm: 2, loc: loc() },
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::MUL,
                loc: loc(),
            },
        ];
        canonicalize_operand_order(&mut ops, &[]);
        assert!(matches!(ops[0], IlOp::Load { slot: 0, .. }));
        assert!(matches!(ops[1], IlOp::Const { imm: 1, .. }));
        assert!(matches!(ops[3], IlOp::Load { slot: 1, .. }));
        assert!(matches!(ops[4], IlOp::Const { imm: 2, .. }));
    }
}
