//! Algebraic / strength-reduction peeps on typed stack IL (Known-SP windows).

use common::Instruction;

use super::op::IlOp;
use super::sp;

/// Exact IEEE-754 `+0.0` / `+1.0` bit patterns (refuse NaN / −0.0).
const F64_PLUS_ZERO: u64 = 0.0_f64.to_bits();
const F64_PLUS_ONE: u64 = 1.0_f64.to_bits();

/// Cheap identity / strength rewrites. Refuses when SP-in mid-window is Unknown.
///
/// `pool` supplies `ConstPool` payloads so float identities / const-fold can
/// read bits and push IEEE results (same push style as lower's wide int fold).
/// Pass an empty vec when the pool is unavailable (int-only peeps still apply).
pub fn algebraic_simplify(ops: &mut Vec<IlOp>, pool: &mut Vec<u64>) {
    if ops.len() < 2 {
        return;
    }
    let info = sp::analyze(ops);
    let mut out = Vec::with_capacity(ops.len());
    let mut i = 0;
    while i < ops.len() {
        if !info.sp_before(i).is_known() {
            out.push(ops[i].clone());
            i += 1;
            continue;
        }

        // Const a; Const b; Bin → Const result for scalar int ops.
        // Refuse results that cannot be encoded as inline CONST (bit 31 is
        // POOL_FLAG — negatives would be misread as pool indices).
        if i + 2 < ops.len()
            && let (IlOp::Const { imm: a, loc }, IlOp::Const { imm: b, .. }, IlOp::Bin { op, .. }) =
                (&ops[i], &ops[i + 1], &ops[i + 2])
            && info.sp_before(i + 1).is_known()
            && info.sp_before(i + 2).is_known()
            && let Some(r) = eval_const_bin(*op, *a, *b)
            && r >= 0
        {
            out.push(IlOp::Const { imm: r, loc: *loc });
            i += 3;
            continue;
        }

        // ConstPool a; ConstPool b; float Bin/Cmp → ConstPool bits or Const 0/1.
        // DIVF/MODF by ±0.0 refused; NaN/Inf kept as deterministic pool bits.
        if i + 2 < ops.len()
            && info.sp_before(i + 1).is_known()
            && info.sp_before(i + 2).is_known()
            && let (IlOp::ConstPool { idx: ia, loc }, IlOp::ConstPool { idx: ib, .. }, IlOp::Bin { op, .. }) =
                (&ops[i], &ops[i + 1], &ops[i + 2])
        {
            let ia = *ia;
            let ib = *ib;
            let loc = *loc;
            let op = *op;
            if let (Some(a), Some(b)) = (
                pool.get(ia as usize).copied(),
                pool.get(ib as usize).copied(),
            ) {
                if let Some(bits) = eval_const_float_bin(op, a, b) {
                    let idx = pool.len() as u32;
                    pool.push(bits);
                    out.push(IlOp::ConstPool { idx, loc });
                    i += 3;
                    continue;
                }
                if let Some(r) = eval_const_float_cmp(op, a, b) {
                    out.push(IlOp::Const { imm: r, loc });
                    i += 3;
                    continue;
                }
            }
        }

        // Double LogicalNot (NOT; NOT) → identity (drop both).
        if i + 1 < ops.len()
            && is_logical_not(&ops[i])
            && is_logical_not(&ops[i + 1])
            && info.sp_before(i + 1).is_known()
        {
            i += 2;
            continue;
        }

        // BinSlotImm identity: slot+0, slot-0, slot*1, slot/1 → Load slot
        if let IlOp::BinSlotImm { op, slot, imm, loc } = &ops[i]
            && let Some(load) = bin_slot_imm_identity(*op, *slot, *imm, *loc)
        {
            out.push(load);
            i += 1;
            continue;
        }

        // Load/Const; Const/Load; Bin identity / zeroing.
        if i + 2 < ops.len()
            && info.sp_before(i + 1).is_known()
            && info.sp_before(i + 2).is_known()
            && let Some(rewritten) = try_bin_identity(&ops[i], &ops[i + 1], &ops[i + 2], pool)
        {
            out.push(rewritten);
            i += 3;
            continue;
        }

        // Load s; Const 2; Pow → Load s; Dup; Mul (square).
        if i + 2 < ops.len()
            && info.sp_before(i + 1).is_known()
            && info.sp_before(i + 2).is_known()
            && let (IlOp::Load { slot, loc }, IlOp::Const { imm: 2, .. }, IlOp::Bin { op, .. }) =
                (&ops[i], &ops[i + 1], &ops[i + 2])
            && *op == Instruction::Pow
        {
            out.push(IlOp::Load {
                slot: *slot,
                loc: *loc,
            });
            out.push(IlOp::Dup { loc: *loc });
            out.push(IlOp::Bin {
                op: Instruction::MUL,
                loc: *loc,
            });
            i += 3;
            continue;
        }

        out.push(ops[i].clone());
        i += 1;
    }
    *ops = out;
}

fn is_logical_not(op: &IlOp) -> bool {
    matches!(op, IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::NOT)
        || matches!(op.as_encode_byte(), Some(b) if *b.bytecode() == Instruction::NOT)
}

fn is_int_cmp(op: Instruction) -> bool {
    matches!(
        op,
        Instruction::EQ
            | Instruction::NEQ
            | Instruction::LE
            | Instruction::LEQ
            | Instruction::GT
            | Instruction::GEQ
    )
}

fn eval_cmp(op: Instruction, a: i32, b: i32) -> i32 {
    let t = match op {
        Instruction::EQ => a == b,
        Instruction::NEQ => a != b,
        Instruction::LE => a < b,
        Instruction::LEQ => a <= b,
        Instruction::GT => a > b,
        Instruction::GEQ => a >= b,
        _ => return 0,
    };
    i32::from(t)
}

/// Fold `Const a; Const b; Bin` when both immediates are inline ints.
fn eval_const_bin(op: Instruction, a: i32, b: i32) -> Option<i32> {
    if is_int_cmp(op) {
        return Some(eval_cmp(op, a, b));
    }
    match op {
        Instruction::ADD => Some(a.wrapping_add(b)),
        Instruction::SUB => Some(a.wrapping_sub(b)),
        Instruction::MUL => Some(a.wrapping_mul(b)),
        Instruction::DIV if b != 0 => Some(a / b),
        Instruction::MOD if b != 0 => Some(a % b),
        Instruction::BITAND => Some(a & b),
        Instruction::BITOR => Some(a | b),
        Instruction::XOR => Some(a ^ b),
        Instruction::SHL if (0..32).contains(&b) => Some(a.wrapping_shl(b as u32)),
        Instruction::SHR if (0..32).contains(&b) => Some(a.wrapping_shr(b as u32)),
        Instruction::AND => Some(i32::from(a != 0 && b != 0)),
        Instruction::OR => Some(i32::from(a != 0 || b != 0)),
        Instruction::Pow if (0..32).contains(&b) => Some(a.wrapping_pow(b as u32)),
        _ => None,
    }
}

/// IEEE float binop bits matching VM `as_float` + `to_bits`. Refuse ÷/% by ±0.0.
fn eval_const_float_bin(op: Instruction, a_bits: u64, b_bits: u64) -> Option<u64> {
    let a = f64::from_bits(a_bits);
    let b = f64::from_bits(b_bits);
    let r = match op {
        Instruction::ADDF => a + b,
        Instruction::SUBF => a - b,
        Instruction::MULF => a * b,
        Instruction::DIVF if b != 0.0 => a / b,
        Instruction::MODF if b != 0.0 => a % b,
        _ => return None,
    };
    Some(r.to_bits())
}

fn is_float_cmp(op: Instruction) -> bool {
    matches!(
        op,
        Instruction::LEF | Instruction::LEQF | Instruction::GTF | Instruction::GEQF
    )
}

/// Float compares → int 0/1 (same as VM `binary!(…, as_float)` without `to_bits`).
fn eval_const_float_cmp(op: Instruction, a_bits: u64, b_bits: u64) -> Option<i32> {
    if !is_float_cmp(op) {
        return None;
    }
    let a = f64::from_bits(a_bits);
    let b = f64::from_bits(b_bits);
    let t = match op {
        Instruction::LEF => a < b,
        Instruction::LEQF => a <= b,
        Instruction::GTF => a > b,
        Instruction::GEQF => a >= b,
        _ => return None,
    };
    Some(i32::from(t))
}

fn bin_slot_imm_identity(op: u8, slot: u8, imm: i16, loc: common::DebugLoc) -> Option<IlOp> {
    let insn = Instruction::from(op);
    let keep = match insn {
        Instruction::ADD | Instruction::SUB if imm == 0 => true,
        Instruction::MUL | Instruction::DIV if imm == 1 => true,
        Instruction::BITOR | Instruction::XOR | Instruction::SHL | Instruction::SHR if imm == 0 => {
            true
        }
        Instruction::BITAND if imm == -1 => true,
        _ => false,
    };
    if keep {
        Some(IlOp::Load {
            slot: slot as u32,
            loc,
        })
    } else if matches!(insn, Instruction::MUL) && imm == 0 {
        Some(IlOp::Const { imm: 0, loc })
    } else if matches!(insn, Instruction::BITAND) && imm == 0 {
        Some(IlOp::Const { imm: 0, loc })
    } else if matches!(insn, Instruction::Pow) && imm == 0 {
        Some(IlOp::Const { imm: 1, loc })
    } else if matches!(insn, Instruction::Pow) && imm == 1 {
        Some(IlOp::Load {
            slot: slot as u32,
            loc,
        })
    } else {
        None
    }
}

/// Ops that can stand alone as a binop stack operand in a 3-op window.
fn is_scalar_producer(op: &IlOp) -> bool {
    matches!(
        op,
        IlOp::Load { .. } | IlOp::Const { .. } | IlOp::ConstPool { .. }
    )
}

fn pool_bits(op: &IlOp, pool: &[u64]) -> Option<u64> {
    match op {
        IlOp::ConstPool { idx, .. } => pool.get(*idx as usize).copied(),
        _ => None,
    }
}

fn is_exact_plus_zero(bits: u64) -> bool {
    bits == F64_PLUS_ZERO
}

fn is_exact_plus_one(bits: u64) -> bool {
    bits == F64_PLUS_ONE
}

fn try_bin_identity(a: &IlOp, b: &IlOp, bin: &IlOp, pool: &[u64]) -> Option<IlOp> {
    let IlOp::Bin { op, loc } = bin else {
        return None;
    };
    let loc = *loc;
    match (*op, a, b) {
        // x + 0 / x - 0 / x * 1 / x / 1 → x
        (Instruction::ADD | Instruction::SUB, x, IlOp::Const { imm: 0, .. })
        | (Instruction::MUL | Instruction::DIV, x, IlOp::Const { imm: 1, .. }) => {
            if is_scalar_producer(x) {
                Some(x.clone())
            } else {
                None
            }
        }
        // 0 + x / 1 * x → x
        (Instruction::ADD, IlOp::Const { imm: 0, .. }, x)
        | (Instruction::MUL, IlOp::Const { imm: 1, .. }, x) => {
            if is_scalar_producer(x) {
                Some(x.clone())
            } else {
                None
            }
        }
        // Float: x + 0.0 / 0.0 + x → x (exact +0.0 pool bits only; refuse −0/NaN).
        // Intentionally no SUBF ±0.0 identity (signed zero).
        (Instruction::ADDF, x, c) | (Instruction::ADDF, c, x)
            if is_scalar_producer(x)
                && pool_bits(c, pool).is_some_and(is_exact_plus_zero) =>
        {
            Some(x.clone())
        }
        // Float: x * 1.0 / 1.0 * x → x (exact +1.0 only; refuse *0.0 / NaN).
        (Instruction::MULF, x, c) | (Instruction::MULF, c, x)
            if is_scalar_producer(x)
                && pool_bits(c, pool).is_some_and(is_exact_plus_one) =>
        {
            Some(x.clone())
        }
        // x | 0 / x ^ 0 / x << 0 / x >> 0 → x
        (
            Instruction::BITOR | Instruction::XOR | Instruction::SHL | Instruction::SHR,
            x,
            IlOp::Const { imm: 0, .. },
        ) => {
            if is_scalar_producer(x) {
                Some(x.clone())
            } else {
                None
            }
        }
        // x & -1 → x; x & 0 → 0
        (Instruction::BITAND, x, IlOp::Const { imm: -1, .. }) => {
            if is_scalar_producer(x) {
                Some(x.clone())
            } else {
                None
            }
        }
        // Zeroing folds require both window ops to be scalar producers. A bare
        // `_` would match `Const 0; Index; MUL` (index literal, not MUL operand).
        (Instruction::BITAND, x, IlOp::Const { imm: 0, .. })
        | (Instruction::BITAND, IlOp::Const { imm: 0, .. }, x)
            if is_scalar_producer(x) =>
        {
            Some(IlOp::Const { imm: 0, loc })
        }
        // x - x / x * 0 / 0 * x → Const 0 (same Load slot or same Const)
        (Instruction::SUB, IlOp::Load { slot: s0, .. }, IlOp::Load { slot: s1, .. })
            if s0 == s1 =>
        {
            Some(IlOp::Const { imm: 0, loc })
        }
        (Instruction::SUB, IlOp::Const { imm: a, .. }, IlOp::Const { imm: b, .. }) if a == b => {
            Some(IlOp::Const { imm: 0, loc })
        }
        (Instruction::MUL, x, IlOp::Const { imm: 0, .. })
        | (Instruction::MUL, IlOp::Const { imm: 0, .. }, x)
            if is_scalar_producer(x) =>
        {
            Some(IlOp::Const { imm: 0, loc })
        }
        // x ** 0 → 1; x ** 1 → x
        (Instruction::Pow, x, IlOp::Const { imm: 0, .. }) if is_scalar_producer(x) => {
            Some(IlOp::Const { imm: 1, loc })
        }
        (Instruction::Pow, x, IlOp::Const { imm: 1, .. }) => {
            if is_scalar_producer(x) {
                Some(x.clone())
            } else {
                None
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::sp::Sp;
    use super::*;
    use common::DebugLoc;

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

    #[test]
    fn add_zero_folds_to_load() {
        let mut ops = vec![
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        algebraic_simplify(&mut ops, &mut Vec::new());
        assert!(matches!(ops[0], IlOp::Load { slot: 1, .. }));
        assert!(matches!(ops[1], IlOp::Return { .. }));
    }

    #[test]
    fn mul_zero_folds_to_const_zero() {
        let mut ops = vec![
            IlOp::Load {
                slot: 2,
                loc: loc(),
            },
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::Bin {
                op: Instruction::MUL,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        algebraic_simplify(&mut ops, &mut Vec::new());
        assert!(matches!(ops[0], IlOp::Const { imm: 0, .. }));
    }

    #[test]
    fn sub_same_load_folds_to_zero() {
        let mut ops = vec![
            IlOp::Load {
                slot: 4,
                loc: loc(),
            },
            IlOp::Load {
                slot: 4,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::SUB,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        algebraic_simplify(&mut ops, &mut Vec::new());
        assert!(matches!(ops[0], IlOp::Const { imm: 0, .. }));
    }

    #[test]
    fn cmp_const_folds() {
        let mut ops = vec![
            IlOp::Const { imm: 3, loc: loc() },
            IlOp::Const { imm: 3, loc: loc() },
            IlOp::Bin {
                op: Instruction::EQ,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        algebraic_simplify(&mut ops, &mut Vec::new());
        assert!(matches!(ops[0], IlOp::Const { imm: 1, .. }));
    }

    #[test]
    fn double_not_eliminated() {
        let mut ops = vec![
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::byte(common::Byte::new(Instruction::NOT)),
            IlOp::byte(common::Byte::new(Instruction::NOT)),
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        algebraic_simplify(&mut ops, &mut Vec::new());
        assert_eq!(ops.len(), 2);
        assert!(matches!(ops[0], IlOp::Const { imm: 1, .. }));
    }

    #[test]
    fn bin_slot_imm_add_zero_to_load() {
        let mut ops = vec![
            IlOp::BinSlotImm {
                op: Instruction::ADD as u8,
                slot: 3,
                imm: 0,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        algebraic_simplify(&mut ops, &mut Vec::new());
        assert!(matches!(ops[0], IlOp::Load { slot: 3, .. }));
    }

    #[test]
    fn refuses_when_sp_unknown() {
        let mut ops = vec![
            IlOp::byte(common::Byte::new(Instruction::FfiInvoke)),
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        let before = ops.clone();
        algebraic_simplify(&mut ops, &mut Vec::new());
        // Window starting at Load has Unknown SP-in after FfiInvoke.
        assert_eq!(ops.len(), before.len());
    }

    #[test]
    fn zero_plus_load_folds_to_load() {
        let mut ops = vec![
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::Load {
                slot: 5,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        algebraic_simplify(&mut ops, &mut Vec::new());
        assert!(matches!(ops[0], IlOp::Load { slot: 5, .. }));
        assert!(matches!(ops[1], IlOp::Return { .. }));
    }

    #[test]
    fn bin_slot_imm_mul_zero_to_const_zero() {
        let mut ops = vec![
            IlOp::BinSlotImm {
                op: Instruction::MUL as u8,
                slot: 2,
                imm: 0,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        algebraic_simplify(&mut ops, &mut Vec::new());
        assert!(matches!(ops[0], IlOp::Const { imm: 0, .. }));
    }

    #[test]
    fn const_pool_plus_zero_folds_to_pool() {
        let mut ops = vec![
            IlOp::ConstPool { idx: 3, loc: loc() },
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        algebraic_simplify(&mut ops, &mut Vec::new());
        assert!(matches!(ops[0], IlOp::ConstPool { idx: 3, .. }));
        assert!(matches!(ops[1], IlOp::Return { .. }));
    }

    #[test]
    fn analyze_known_gate_smoke() {
        let ops = vec![IlOp::Const { imm: 1, loc: loc() }];
        assert!(matches!(sp::analyze(&ops).sp_before(0), Sp::Known(0)));
    }

    #[test]
    fn bitand_minus_one_folds_to_load() {
        let mut ops = vec![
            IlOp::Load {
                slot: 2,
                loc: loc(),
            },
            IlOp::Const {
                imm: -1,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::BITAND,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        algebraic_simplify(&mut ops, &mut Vec::new());
        assert!(matches!(ops[0], IlOp::Load { slot: 2, .. }));
        assert!(matches!(ops[1], IlOp::Return { .. }));
    }

    #[test]
    fn pow_square_becomes_dup_mul() {
        let mut ops = vec![
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::Const { imm: 2, loc: loc() },
            IlOp::Bin {
                op: Instruction::Pow,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        algebraic_simplify(&mut ops, &mut Vec::new());
        assert!(matches!(ops[0], IlOp::Load { slot: 1, .. }));
        assert!(matches!(ops[1], IlOp::Dup { .. }));
        assert!(matches!(
            ops[2],
            IlOp::Bin {
                op: Instruction::MUL,
                ..
            }
        ));
    }

    #[test]
    fn pow_zero_folds_to_one() {
        let mut ops = vec![
            IlOp::Load {
                slot: 3,
                loc: loc(),
            },
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::Bin {
                op: Instruction::Pow,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        algebraic_simplify(&mut ops, &mut Vec::new());
        assert!(matches!(ops[0], IlOp::Const { imm: 1, .. }));
    }

    #[test]
    fn const_const_binop_folds() {
        let mut ops = vec![
            IlOp::Const { imm: 6, loc: loc() },
            IlOp::Const { imm: 7, loc: loc() },
            IlOp::Bin {
                op: Instruction::MUL,
                loc: loc(),
            },
            IlOp::Const { imm: 3, loc: loc() },
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Bin {
                op: Instruction::BITAND,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        algebraic_simplify(&mut ops, &mut Vec::new());
        assert!(matches!(ops[0], IlOp::Const { imm: 42, .. }));
        assert!(matches!(ops[1], IlOp::Const { imm: 1, .. }));
        assert!(matches!(ops[2], IlOp::Return { .. }));
    }

    #[test]
    fn const_const_div_mod_zero_and_wide_shift_refused() {
        let mut div0 = vec![
            IlOp::Const { imm: 8, loc: loc() },
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::Bin {
                op: Instruction::DIV,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        let before = div0.clone();
        algebraic_simplify(&mut div0, &mut Vec::new());
        assert_eq!(div0.len(), before.len(), "DIV by 0 must not fold");

        let mut mod0 = vec![
            IlOp::Const { imm: 8, loc: loc() },
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::Bin {
                op: Instruction::MOD,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        let before = mod0.clone();
        algebraic_simplify(&mut mod0, &mut Vec::new());
        assert_eq!(mod0.len(), before.len(), "MOD by 0 must not fold");

        let mut shl = vec![
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Const {
                imm: 32,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::SHL,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        let before = shl.clone();
        algebraic_simplify(&mut shl, &mut Vec::new());
        assert_eq!(shl.len(), before.len(), "SHL amount ≥ 32 must not fold");
    }

    #[test]
    fn pow_one_identity_and_bitand_zero() {
        let mut pow1 = vec![
            IlOp::Load {
                slot: 2,
                loc: loc(),
            },
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Bin {
                op: Instruction::Pow,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        algebraic_simplify(&mut pow1, &mut Vec::new());
        assert!(matches!(pow1[0], IlOp::Load { slot: 2, .. }));
        assert!(matches!(pow1[1], IlOp::Return { .. }));

        let mut and0 = vec![
            IlOp::Load {
                slot: 3,
                loc: loc(),
            },
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::Bin {
                op: Instruction::BITAND,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        algebraic_simplify(&mut and0, &mut Vec::new());
        assert!(matches!(and0[0], IlOp::Const { imm: 0, .. }));
    }

    #[test]
    fn refuses_fold_to_negative_inline_const() {
        // 0 - 1 → -1 cannot be IlOp::Const (POOL_FLAG bit collision).
        let mut ops = vec![
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Bin {
                op: Instruction::SUB,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        let before_len = ops.len();
        algebraic_simplify(&mut ops, &mut Vec::new());
        assert_eq!(
            ops.len(),
            before_len,
            "negative fold result must stay Const;Const;SUB"
        );
        assert!(matches!(
            ops[2],
            IlOp::Bin {
                op: Instruction::SUB,
                ..
            }
        ));
    }

    #[test]
    fn refuses_mul_zero_fold_through_index() {
        // `2 * t[0]` is Load; Load; Const 0; Index; MUL — Const 0 indexes, not multiplies.
        let mut ops = vec![
            IlOp::Load {
                slot: 11,
                loc: loc(),
            },
            IlOp::Load {
                slot: 10,
                loc: loc(),
            },
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::Index { loc: loc() },
            IlOp::Bin {
                op: Instruction::MUL,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        algebraic_simplify(&mut ops, &mut Vec::new());
        assert!(
            matches!(ops[3], IlOp::Index { .. }),
            "Index must survive Const 0; Index; MUL"
        );
        assert!(
            matches!(
                ops[4],
                IlOp::Bin {
                    op: Instruction::MUL,
                    ..
                }
            ),
            "MUL must survive Const 0; Index; MUL"
        );
    }

    fn pool_with(bits: &[u64]) -> Vec<u64> {
        bits.to_vec()
    }

    #[test]
    fn float_add_plus_zero_folds_to_load() {
        let mut pool = pool_with(&[F64_PLUS_ZERO]);
        let mut ops = vec![
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::ConstPool { idx: 0, loc: loc() },
            IlOp::Bin {
                op: Instruction::ADDF,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        algebraic_simplify(&mut ops, &mut pool);
        assert!(matches!(ops[0], IlOp::Load { slot: 1, .. }));
        assert!(matches!(ops[1], IlOp::Return { .. }));
    }

    #[test]
    fn float_plus_zero_add_load_folds() {
        let mut pool = pool_with(&[F64_PLUS_ZERO]);
        let mut ops = vec![
            IlOp::ConstPool { idx: 0, loc: loc() },
            IlOp::Load {
                slot: 4,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::ADDF,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        algebraic_simplify(&mut ops, &mut pool);
        assert!(matches!(ops[0], IlOp::Load { slot: 4, .. }));
        assert!(matches!(ops[1], IlOp::Return { .. }));
    }

    #[test]
    fn float_mul_plus_one_folds_to_load() {
        let mut pool = pool_with(&[F64_PLUS_ONE]);
        let mut ops = vec![
            IlOp::Load {
                slot: 2,
                loc: loc(),
            },
            IlOp::ConstPool { idx: 0, loc: loc() },
            IlOp::Bin {
                op: Instruction::MULF,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        algebraic_simplify(&mut ops, &mut pool);
        assert!(matches!(ops[0], IlOp::Load { slot: 2, .. }));
        assert!(matches!(ops[1], IlOp::Return { .. }));
    }

    #[test]
    fn float_plus_one_mul_load_folds() {
        let mut pool = pool_with(&[F64_PLUS_ONE]);
        let mut ops = vec![
            IlOp::ConstPool { idx: 0, loc: loc() },
            IlOp::Load {
                slot: 3,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::MULF,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        algebraic_simplify(&mut ops, &mut pool);
        assert!(matches!(ops[0], IlOp::Load { slot: 3, .. }));
        assert!(matches!(ops[1], IlOp::Return { .. }));
    }

    #[test]
    fn float_refuses_subf_plus_zero() {
        let mut pool = pool_with(&[F64_PLUS_ZERO]);
        let mut ops = vec![
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::ConstPool { idx: 0, loc: loc() },
            IlOp::Bin {
                op: Instruction::SUBF,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        let before = ops.clone();
        algebraic_simplify(&mut ops, &mut pool);
        assert_eq!(ops.len(), before.len(), "x - 0.0 must not fold (signed zero)");
        assert!(matches!(
            ops[2],
            IlOp::Bin {
                op: Instruction::SUBF,
                ..
            }
        ));
    }

    #[test]
    fn float_refuses_mulf_plus_zero() {
        let mut pool = pool_with(&[F64_PLUS_ZERO]);
        let mut ops = vec![
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::ConstPool { idx: 0, loc: loc() },
            IlOp::Bin {
                op: Instruction::MULF,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        let before = ops.clone();
        algebraic_simplify(&mut ops, &mut pool);
        assert_eq!(ops.len(), before.len(), "x * 0.0 must not fold (NaN/−0)");
    }

    #[test]
    fn float_refuses_neg_zero_and_nan() {
        let neg_zero = (-0.0_f64).to_bits();
        let nan = f64::NAN.to_bits();
        let mut pool = pool_with(&[neg_zero, nan]);

        let mut neg = vec![
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::ConstPool { idx: 0, loc: loc() },
            IlOp::Bin {
                op: Instruction::ADDF,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        let before_neg = neg.len();
        algebraic_simplify(&mut neg, &mut pool);
        assert_eq!(neg.len(), before_neg, "−0.0 must not fold");

        let mut nan_ops = vec![
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::ConstPool { idx: 1, loc: loc() },
            IlOp::Bin {
                op: Instruction::ADDF,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        let before_nan = nan_ops.len();
        algebraic_simplify(&mut nan_ops, &mut pool);
        assert_eq!(nan_ops.len(), before_nan, "NaN must not fold");
    }

    #[test]
    fn float_refuses_without_pool_bits() {
        // ConstPool idx present but pool empty / missing — unknown constant.
        let mut ops = vec![
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::ConstPool { idx: 0, loc: loc() },
            IlOp::Bin {
                op: Instruction::ADDF,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        let before = ops.len();
        algebraic_simplify(&mut ops, &mut Vec::new());
        assert_eq!(ops.len(), before, "missing pool bits must refuse float fold");
    }

    #[test]
    fn float_refuses_non_const_rhs() {
        let mut ops = vec![
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::Load {
                slot: 2,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::ADDF,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        let before = ops.len();
        algebraic_simplify(&mut ops, &mut vec![F64_PLUS_ZERO]);
        assert_eq!(ops.len(), before, "non-const RHS must refuse float identity");
    }

    #[test]
    fn float_const_pool_add_folds_to_pool_result() {
        let mut pool = pool_with(&[1.5_f64.to_bits(), 2.5_f64.to_bits()]);
        let mut ops = vec![
            IlOp::ConstPool { idx: 0, loc: loc() },
            IlOp::ConstPool { idx: 1, loc: loc() },
            IlOp::Bin {
                op: Instruction::ADDF,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        algebraic_simplify(&mut ops, &mut pool);
        assert!(matches!(ops[0], IlOp::ConstPool { idx: 2, .. }));
        assert!(matches!(ops[1], IlOp::Return { .. }));
        assert_eq!(pool[2], 4.0_f64.to_bits());
    }

    #[test]
    fn float_const_pool_arith_ops_fold() {
        let a = 8.0_f64.to_bits();
        let b = 2.0_f64.to_bits();
        for (op, expect) in [
            (Instruction::SUBF, 6.0_f64),
            (Instruction::MULF, 16.0_f64),
            (Instruction::DIVF, 4.0_f64),
            (Instruction::MODF, 0.0_f64),
        ] {
            let mut pool = pool_with(&[a, b]);
            let mut ops = vec![
                IlOp::ConstPool { idx: 0, loc: loc() },
                IlOp::ConstPool { idx: 1, loc: loc() },
                IlOp::Bin { op, loc: loc() },
                IlOp::Return { loc: loc(), ret_words: 1},
            ];
            algebraic_simplify(&mut ops, &mut pool);
            assert!(
                matches!(ops[0], IlOp::ConstPool { idx: 2, .. }),
                "{op:?} should fold to ConstPool"
            );
            assert_eq!(pool[2], expect.to_bits(), "{op:?} bits");
        }
    }

    #[test]
    fn float_const_pool_div_mod_zero_refused() {
        let a = 1.0_f64.to_bits();
        for (zero, op) in [
            (0.0_f64.to_bits(), Instruction::DIVF),
            ((-0.0_f64).to_bits(), Instruction::DIVF),
            (0.0_f64.to_bits(), Instruction::MODF),
            ((-0.0_f64).to_bits(), Instruction::MODF),
        ] {
            let mut pool = pool_with(&[a, zero]);
            let mut ops = vec![
                IlOp::ConstPool { idx: 0, loc: loc() },
                IlOp::ConstPool { idx: 1, loc: loc() },
                IlOp::Bin { op, loc: loc() },
                IlOp::Return { loc: loc(), ret_words: 1},
            ];
            let before = ops.len();
            algebraic_simplify(&mut ops, &mut pool);
            assert_eq!(ops.len(), before, "{op:?} by ±0.0 must not fold");
            assert_eq!(pool.len(), 2, "pool must not grow on refuse");
        }
    }

    #[test]
    fn float_const_pool_nan_inf_kept_as_bits() {
        let nan = f64::NAN.to_bits();
        let mut pool = pool_with(&[nan, 1.0_f64.to_bits()]);
        let mut ops = vec![
            IlOp::ConstPool { idx: 0, loc: loc() },
            IlOp::ConstPool { idx: 1, loc: loc() },
            IlOp::Bin {
                op: Instruction::ADDF,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        algebraic_simplify(&mut ops, &mut pool);
        assert!(matches!(ops[0], IlOp::ConstPool { idx: 2, .. }));
        assert!(f64::from_bits(pool[2]).is_nan());

        let mut pool_inf = pool_with(&[f64::INFINITY.to_bits(), 1.0_f64.to_bits()]);
        let mut ops_inf = vec![
            IlOp::ConstPool { idx: 0, loc: loc() },
            IlOp::ConstPool { idx: 1, loc: loc() },
            IlOp::Bin {
                op: Instruction::MULF,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        algebraic_simplify(&mut ops_inf, &mut pool_inf);
        assert!(matches!(ops_inf[0], IlOp::ConstPool { idx: 2, .. }));
        assert_eq!(pool_inf[2], f64::INFINITY.to_bits());
    }

    #[test]
    fn float_const_pool_cmp_folds_to_inline_const() {
        let mut pool = pool_with(&[1.0_f64.to_bits(), 2.0_f64.to_bits()]);
        let mut ops = vec![
            IlOp::ConstPool { idx: 0, loc: loc() },
            IlOp::ConstPool { idx: 1, loc: loc() },
            IlOp::Bin {
                op: Instruction::LEF,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        algebraic_simplify(&mut ops, &mut pool);
        assert!(matches!(ops[0], IlOp::Const { imm: 1, .. }));
        assert!(matches!(ops[1], IlOp::Return { .. }));
        assert_eq!(pool.len(), 2, "cmp fold must not push pool bits");

        let mut pool2 = pool_with(&[3.0_f64.to_bits(), 1.0_f64.to_bits()]);
        let mut gtf = vec![
            IlOp::ConstPool { idx: 0, loc: loc() },
            IlOp::ConstPool { idx: 1, loc: loc() },
            IlOp::Bin {
                op: Instruction::GTF,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        algebraic_simplify(&mut gtf, &mut pool2);
        assert!(matches!(gtf[0], IlOp::Const { imm: 1, .. }));
    }

    #[test]
    fn float_const_pool_refuses_reassoc_across_non_const() {
        // Load; ConstPool; ADDF must not fold even with a pool float nearby.
        let mut pool = pool_with(&[1.5_f64.to_bits(), 2.5_f64.to_bits()]);
        let mut ops = vec![
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::ConstPool { idx: 0, loc: loc() },
            IlOp::Bin {
                op: Instruction::ADDF,
                loc: loc(),
            },
            IlOp::ConstPool { idx: 1, loc: loc() },
            IlOp::Bin {
                op: Instruction::ADDF,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        let before_len = ops.len();
        algebraic_simplify(&mut ops, &mut pool);
        assert_eq!(ops.len(), before_len);
        assert!(matches!(ops[0], IlOp::Load { .. }));
        assert!(matches!(
            ops[2],
            IlOp::Bin {
                op: Instruction::ADDF,
                ..
            }
        ));
        assert_eq!(pool.len(), 2);
    }

    #[test]
    fn float_const_pool_add_via_optimize_pipeline() {
        use super::super::opt::{OptimizeOptions, optimize};
        let mut pool = pool_with(&[1.5_f64.to_bits(), 2.5_f64.to_bits()]);
        let mut ops = vec![
            IlOp::ConstPool { idx: 0, loc: loc() },
            IlOp::ConstPool { idx: 1, loc: loc() },
            IlOp::Bin {
                op: Instruction::ADDF,
                loc: loc(),
            },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        optimize(
            &mut ops,
            &OptimizeOptions {
                jump_thread: false,
                dead_block: false,
                stack_dce: false,
                mem_fwd: false,
                copy_prop: false,
                slot_promote: false,
                tos_carry: false,
                canon: false,
                cast_spill: false,
                algebraic: true,
                instcombine: false,
                local_cse: false,
                licm: false,
                loop_bounds: false,
                return_convoy: false,
                clone_shared_return: false,
                bin_join_convoy: false,
                multi_op_join_convoy: false,
                invert_guard_branch: false,
                slot_promote_tell: false,
                seek_back_edge: false,
                loop_unroll: false,
                loop_unroll_factor: 8,
                invariant_store_elim: false,
                ssa_gvn: false,
                escape_analysis: false,
                branch_optimization: false,
                block_reordering: false,
                iterative_optimization: false,
                max_optimization_iterations: 10,
                collect_stats: false,
                pure_call_ctx: None,
            },
            &mut pool,
        );
        assert!(matches!(ops[0], IlOp::ConstPool { idx: 2, .. }));
        assert!(matches!(ops[1], IlOp::Return { .. }));
        assert_eq!(pool[2], 4.0_f64.to_bits());
    }
}
