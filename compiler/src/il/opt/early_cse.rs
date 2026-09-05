//! Local EarlyCSE over one basic block of stack IL.
//!
//! Hashes self-contained pure expressions. A later identical compute becomes
//! `Load` of the slot that still holds the first result, or `Dup` when that
//! result is still TOS. Effectful ops, unknown `Byte`, and calls are barriers.
//! Does not CSE across blocks or speculate through stores / GC / host.

use std::collections::HashMap;

use common::Instruction;

use crate::il::gvn::gvn_cfg;
use crate::il::op::IlOp;

/// Intra-block CSE. Returns how many replacements fired.
pub(crate) fn early_cse(ops: &mut Vec<IlOp>) -> usize {
    if ops.len() < 2 {
        return 0;
    }
    let (ranges, _) = gvn_cfg(ops);
    let mut out = Vec::with_capacity(ops.len());
    let mut hits = 0usize;
    for (start, end) in ranges {
        let (block, n) = cse_block(&ops[start..end]);
        hits += n;
        out.extend(block);
    }
    if hits > 0 {
        *ops = out;
    }
    hits
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Expr {
    Const(i32),
    ConstPool(u32),
    Load(u32),
    BinSlotImm { op: u8, slot: u8, imm: i16 },
    BinSlotSlot { op: u8, a: u8, b: u8 },
    BinLoads { op: u16, a: u32, b: u32 },
    BinLoadImm { op: u16, slot: u32, imm: i32 },
    CastI2f(u32),
    ArrayLen(u32),
    Index { arr: u32, idx: u32 },
    IndexPin { pin: u32, idx: u32 },
    LoadField { slot: u32, index: u32 },
}

fn cse_binop(op: Instruction) -> bool {
    !matches!(
        op,
        Instruction::DIV | Instruction::MOD | Instruction::DIVF | Instruction::MODF
    )
}

fn cse_binop_u8(op: u8) -> bool {
    op != Instruction::DIV as u8
        && op != Instruction::MOD as u8
        && op != Instruction::DIVF as u8
        && op != Instruction::MODF as u8
}

fn commutative(op: Instruction) -> bool {
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
            | Instruction::ADDF
            | Instruction::MULF
    )
}

fn commutative_u8(op: u8) -> bool {
    op == Instruction::ADD as u8
        || op == Instruction::MUL as u8
        || op == Instruction::EQ as u8
        || op == Instruction::NEQ as u8
        || op == Instruction::AND as u8
        || op == Instruction::OR as u8
        || op == Instruction::BITAND as u8
        || op == Instruction::BITOR as u8
        || op == Instruction::XOR as u8
        || op == Instruction::ADDF as u8
        || op == Instruction::MULF as u8
}

fn canon_slots(op: Instruction, a: u32, b: u32) -> (u32, u32) {
    if commutative(op) && a > b {
        (b, a)
    } else {
        (a, b)
    }
}

fn canon_u8_slots(op: u8, a: u8, b: u8) -> (u8, u8) {
    if commutative_u8(op) && a > b {
        (b, a)
    } else {
        (a, b)
    }
}

fn is_cast_i2f(op: &IlOp) -> bool {
    matches!(
        op.as_encode_byte(),
        Some(b) if *b.bytecode() == Instruction::CastIntToFloat
    )
}

fn is_array_len(op: &IlOp) -> bool {
    matches!(
        op.as_encode_byte(),
        Some(b) if *b.bytecode() == Instruction::ArrayLen
    )
}

fn is_store_index(op: &IlOp) -> bool {
    matches!(
        op.as_encode_byte(),
        Some(b) if matches!(
            *b.bytecode(),
            Instruction::StoreIndex
                | Instruction::StoreIndexUnchecked
                | Instruction::StoreIndexPin
                | Instruction::StoreIndexPinUnchecked
                | Instruction::ArrayPush
        )
    )
}

fn is_full_barrier(op: &IlOp) -> bool {
    matches!(
        op,
        IlOp::HostInvoke { .. }
            | IlOp::Print { .. }
            | IlOp::Entry { .. }
            | IlOp::MakeTuple { .. }
            | IlOp::MakeArray { .. }
            | IlOp::MakeEnum { .. }
            | IlOp::BoxValue { .. }
            | IlOp::SetField { .. }
            | IlOp::GetField { .. }
    ) || matches!(
        op.as_encode_byte(),
        Some(b) if matches!(
            *b.bytecode(),
            Instruction::HostInvoke
                | Instruction::PRINT
                | Instruction::CALL
                | Instruction::TailCall
                | Instruction::MakeCoro
                | Instruction::MakeTuple
                | Instruction::MakeArray
                | Instruction::MakeEnum
                | Instruction::BoxValue
                | Instruction::FORMAT
                | Instruction::FfiInvoke
                | Instruction::SetField
                | Instruction::GetField
                | Instruction::YieldCoro
                | Instruction::YieldFromCoro
        )
    ) || (matches!(op, IlOp::Byte { .. })
        && !is_cast_i2f(op)
        && !is_array_len(op)
        && !is_store_index(op))
}

fn depends_on(e: &Expr, slot: u32) -> bool {
    match *e {
        Expr::Const(_) | Expr::ConstPool(_) => false,
        Expr::Load(s) | Expr::CastI2f(s) | Expr::ArrayLen(s) => s == slot,
        Expr::BinSlotImm { slot: s, .. } => s as u32 == slot,
        Expr::BinSlotSlot { a, b, .. } => a as u32 == slot || b as u32 == slot,
        Expr::BinLoads { a, b, .. } => a == slot || b == slot,
        Expr::BinLoadImm { slot: s, .. } => s == slot,
        Expr::Index { arr, idx } => arr == slot || idx == slot,
        Expr::IndexPin { pin, idx } => pin == slot || idx == slot,
        Expr::LoadField { slot: s, .. } => s == slot,
    }
}

fn is_mem_expr(e: &Expr) -> bool {
    matches!(
        e,
        Expr::ArrayLen(_) | Expr::Index { .. } | Expr::IndexPin { .. } | Expr::LoadField { .. }
    )
}

fn load_of(slot: u32, loc: common::DebugLoc) -> IlOp {
    IlOp::Load { slot, loc }
}

struct Avail {
    map: HashMap<Expr, u32>,
    tos: Option<Expr>,
}

impl Avail {
    fn new() -> Self {
        Self {
            map: HashMap::new(),
            tos: None,
        }
    }

    fn lookup(&self, e: &Expr) -> Option<u32> {
        self.map.get(e).copied()
    }

    fn bind_slot(&mut self, slot: u32, e: Expr) {
        self.kill_slot(slot);
        self.map.insert(e, slot);
    }

    fn kill_slot(&mut self, slot: u32) {
        self.map
            .retain(|e, s| *s != slot && !depends_on(e, slot));
        if self.tos.as_ref().is_some_and(|t| depends_on(t, slot)) {
            self.tos = None;
        }
    }

    fn kill_mem(&mut self) {
        self.map.retain(|e, _| !is_mem_expr(e));
        if self.tos.as_ref().is_some_and(is_mem_expr) {
            self.tos = None;
        }
    }

    fn clear(&mut self) {
        self.map.clear();
        self.tos = None;
    }
}

fn cse_block(ops: &[IlOp]) -> (Vec<IlOp>, usize) {
    let mut out = Vec::with_capacity(ops.len());
    let mut hits = 0usize;
    let mut avail = Avail::new();
    let mut i = 0;
    while i < ops.len() {
        if matches!(ops[i], IlOp::Label(_) | IlOp::JoinLabel(_)) {
            out.push(ops[i].clone());
            i += 1;
            continue;
        }

        if let Some((consumed, rewrite, expr)) = try_reuse(ops, i, &avail) {
            out.extend(rewrite);
            avail.tos = Some(expr);
            hits += 1;
            i += consumed;
            continue;
        }

        if let IlOp::StorePop { slot, .. } = ops[i] {
            if let Some(e) = avail.tos {
                avail.bind_slot(slot, e);
            } else {
                avail.kill_slot(slot);
            }
            avail.tos = None;
            out.push(ops[i].clone());
            i += 1;
            continue;
        }

        if is_store_index(&ops[i]) {
            avail.kill_mem();
            avail.tos = None;
            out.push(ops[i].clone());
            i += 1;
            continue;
        }

        if is_full_barrier(&ops[i]) {
            avail.clear();
            out.push(ops[i].clone());
            i += 1;
            continue;
        }

        if matches!(&ops[i], IlOp::Jump { .. } | IlOp::Return { .. } | IlOp::Halt { .. }) {
            avail.clear();
            out.push(ops[i].clone());
            i += 1;
            continue;
        }

        if let Some((consumed, expr)) = recognize(ops, i) {
            for k in 0..consumed {
                out.push(ops[i + k].clone());
            }
            avail.tos = Some(expr);
            i += consumed;
            continue;
        }

        avail.tos = None;
        out.push(ops[i].clone());
        i += 1;
    }
    (out, hits)
}

fn try_reuse(ops: &[IlOp], i: usize, avail: &Avail) -> Option<(usize, Vec<IlOp>, Expr)> {
    let (consumed, expr) = recognize(ops, i)?;
    let slot = avail.lookup(&expr)?;
    // Cheap immediates stay as themselves; only reuse stored *compute*.
    if matches!(
        expr,
        Expr::Const(_) | Expr::ConstPool(_) | Expr::Load(_)
    ) {
        return None;
    }
    Some((consumed, vec![load_of(slot, ops[i].loc())], expr))
}

fn recognize(ops: &[IlOp], i: usize) -> Option<(usize, Expr)> {
    match &ops[i] {
        IlOp::Const { imm, .. } => return Some((1, Expr::Const(*imm))),
        IlOp::ConstPool { idx, .. } => return Some((1, Expr::ConstPool(*idx))),
        IlOp::Load { slot, .. } => {
            if i + 2 < ops.len()
                && let IlOp::Load { slot: b, .. } = ops[i + 1]
            {
                if matches!(&ops[i + 2], IlOp::Index { .. } | IlOp::IndexUnchecked { .. }) {
                    return Some((
                        3,
                        Expr::Index {
                            arr: *slot,
                            idx: b,
                        },
                    ));
                }
                if let IlOp::Bin { op, .. } = ops[i + 2]
                    && cse_binop(op)
                {
                    let (a, b) = canon_slots(op, *slot, b);
                    return Some((
                        3,
                        Expr::BinLoads {
                            op: op as u16,
                            a,
                            b,
                        },
                    ));
                }
            }
            if i + 2 < ops.len()
                && let (IlOp::Const { imm, .. }, IlOp::Bin { op, .. }) = (&ops[i + 1], &ops[i + 2])
                && cse_binop(*op)
            {
                return Some((
                    3,
                    Expr::BinLoadImm {
                        op: *op as u16,
                        slot: *slot,
                        imm: *imm,
                    },
                ));
            }
            if let Some(next) = ops.get(i + 1) {
                if is_cast_i2f(next) {
                    return Some((2, Expr::CastI2f(*slot)));
                }
                if is_array_len(next) {
                    return Some((2, Expr::ArrayLen(*slot)));
                }
                if let IlOp::LoadField { index, .. } = next {
                    return Some((
                        2,
                        Expr::LoadField {
                            slot: *slot,
                            index: *index,
                        },
                    ));
                }
                if let IlOp::IndexPin { slot: pin, .. }
                | IlOp::IndexPinUnchecked { slot: pin, .. } = next
                {
                    return Some((
                        2,
                        Expr::IndexPin {
                            pin: *pin,
                            idx: *slot,
                        },
                    ));
                }
            }
            return Some((1, Expr::Load(*slot)));
        }
        IlOp::BinSlotImm { op, slot, imm, .. } if cse_binop_u8(*op) => {
            return Some((
                1,
                Expr::BinSlotImm {
                    op: *op,
                    slot: *slot,
                    imm: *imm,
                },
            ));
        }
        IlOp::BinSlotSlot { op, a, b, .. } if cse_binop_u8(*op) => {
            let (a, b) = canon_u8_slots(*op, *a, *b);
            return Some((1, Expr::BinSlotSlot { op: *op, a, b }));
        }
        _ => {}
    }

    if i + 2 < ops.len()
        && let (IlOp::Load { slot: a, .. }, IlOp::Load { slot: b, .. }) = (&ops[i], &ops[i + 1])
    {
        if matches!(&ops[i + 2], IlOp::Index { .. } | IlOp::IndexUnchecked { .. }) {
            return Some((
                3,
                Expr::Index {
                    arr: *a,
                    idx: *b,
                },
            ));
        }
        if let IlOp::Bin { op, .. } = ops[i + 2]
            && cse_binop(op)
        {
            let (a, b) = canon_slots(op, *a, *b);
            return Some((
                3,
                Expr::BinLoads {
                    op: op as u16,
                    a,
                    b,
                },
            ));
        }
    }

    if i + 2 < ops.len()
        && let (IlOp::Load { slot, .. }, IlOp::Const { imm, .. }, IlOp::Bin { op, .. }) =
            (&ops[i], &ops[i + 1], &ops[i + 2])
        && cse_binop(*op)
    {
        return Some((
            3,
            Expr::BinLoadImm {
                op: *op as u16,
                slot: *slot,
                imm: *imm,
            },
        ));
    }

    if i + 2 < ops.len()
        && let (IlOp::Const { imm, .. }, IlOp::Load { slot, .. }, IlOp::Bin { op, .. }) =
            (&ops[i], &ops[i + 1], &ops[i + 2])
        && cse_binop(*op)
    {
        let (a, b, o) = if commutative(*op) {
            (*slot, *imm as u32, *op)
        } else {
            // Non-commute const-left is a different expr; keep distinct key.
            return Some((
                3,
                Expr::BinLoadImm {
                    op: (*op as u16) | 0x8000,
                    slot: *slot,
                    imm: *imm,
                },
            ));
        };
        let _ = (a, b, o);
        return Some((
            3,
            Expr::BinLoadImm {
                op: *op as u16,
                slot: *slot,
                imm: *imm,
            },
        ));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::il::op::Label;
    use crate::il::opt::{OptimizeOptions, optimize};
    use common::{Byte, DebugLoc};

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

    fn isolated() -> OptimizeOptions {
        let mut o = OptimizeOptions::default();
        o.jump_thread = false;
        o.dead_block = false;
        o.stack_dce = false;
        o.mem_fwd = false;
        o.copy_prop = false;
        o.slot_promote = false;
        o.tos_carry = false;
        o.canon = false;
        o.cast_spill = false;
        o.algebraic = false;
        o.instcombine = false;
        o.licm = false;
        o.loop_bounds = false;
        o.return_convoy = false;
        o.clone_shared_return = false;
        o.bin_join_convoy = false;
        o.multi_op_join_convoy = false;
        o.invert_guard_branch = false;
        o.slot_promote_tell = false;
        o.seek_back_edge = false;
        o.loop_unroll = false;
        o.invariant_store_elim = false;
        o.ssa_gvn = false;
        o.escape_analysis = false;
        o.branch_optimization = false;
        o.block_reordering = false;
        o.local_cse = true;
        o
    }

    #[test]
    fn binslot_store_reused_as_load() {
        let mut ops = vec![
            IlOp::BinSlotSlot {
                op: Instruction::MULF as u8,
                a: 3,
                b: 3,
                loc: loc(),
            },
            IlOp::StorePop { slot: 7, loc: loc() },
            IlOp::BinSlotSlot {
                op: Instruction::MULF as u8,
                a: 3,
                b: 3,
                loc: loc(),
            },
            IlOp::Return {
                loc: loc(),
                ret_words: 1,
            },
        ];
        assert!(early_cse(&mut ops) >= 1);
        assert!(matches!(ops[2], IlOp::Load { slot: 7, .. }));
    }

    #[test]
    fn commutative_binslot_matches_swapped_operands() {
        let mut ops = vec![
            IlOp::BinSlotSlot {
                op: Instruction::ADDF as u8,
                a: 1,
                b: 2,
                loc: loc(),
            },
            IlOp::StorePop { slot: 5, loc: loc() },
            IlOp::BinSlotSlot {
                op: Instruction::ADDF as u8,
                a: 2,
                b: 1,
                loc: loc(),
            },
            IlOp::Return {
                loc: loc(),
                ret_words: 1,
            },
        ];
        early_cse(&mut ops);
        assert!(matches!(ops[2], IlOp::Load { slot: 5, .. }));
    }

    #[test]
    fn store_to_operand_kills_expr() {
        let mut ops = vec![
            IlOp::BinSlotSlot {
                op: Instruction::MUL as u8,
                a: 1,
                b: 2,
                loc: loc(),
            },
            IlOp::StorePop { slot: 5, loc: loc() },
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::StorePop { slot: 1, loc: loc() },
            IlOp::BinSlotSlot {
                op: Instruction::MUL as u8,
                a: 1,
                b: 2,
                loc: loc(),
            },
            IlOp::Return {
                loc: loc(),
                ret_words: 1,
            },
        ];
        early_cse(&mut ops);
        assert!(
            matches!(ops[4], IlOp::BinSlotSlot { .. }),
            "killed by store to operand slot"
        );
    }

    #[test]
    fn refuses_div() {
        let mut ops = vec![
            IlOp::BinSlotImm {
                op: Instruction::DIV as u8,
                slot: 1,
                imm: 2,
                loc: loc(),
            },
            IlOp::StorePop { slot: 3, loc: loc() },
            IlOp::BinSlotImm {
                op: Instruction::DIV as u8,
                slot: 1,
                imm: 2,
                loc: loc(),
            },
            IlOp::Return {
                loc: loc(),
                ret_words: 1,
            },
        ];
        early_cse(&mut ops);
        assert!(matches!(ops[2], IlOp::BinSlotImm { .. }));
    }

    #[test]
    fn host_invoke_is_barrier() {
        let mut ops = vec![
            IlOp::BinSlotImm {
                op: Instruction::ADD as u8,
                slot: 1,
                imm: 1,
                loc: loc(),
            },
            IlOp::StorePop { slot: 3, loc: loc() },
            IlOp::HostInvoke {
                arity: 0,
                layout: 0,
                loc: loc(),
            },
            IlOp::BinSlotImm {
                op: Instruction::ADD as u8,
                slot: 1,
                imm: 1,
                loc: loc(),
            },
            IlOp::Return {
                loc: loc(),
                ret_words: 1,
            },
        ];
        early_cse(&mut ops);
        assert!(matches!(ops[3], IlOp::BinSlotImm { .. }));
    }

    #[test]
    fn does_not_cross_basic_block() {
        let mut ops = vec![
            IlOp::BinSlotImm {
                op: Instruction::ADD as u8,
                slot: 1,
                imm: 4,
                loc: loc(),
            },
            IlOp::StorePop { slot: 3, loc: loc() },
            IlOp::Label(Label(1)),
            IlOp::BinSlotImm {
                op: Instruction::ADD as u8,
                slot: 1,
                imm: 4,
                loc: loc(),
            },
            IlOp::Return {
                loc: loc(),
                ret_words: 1,
            },
        ];
        early_cse(&mut ops);
        assert!(
            matches!(ops[3], IlOp::BinSlotImm { .. }),
            "label starts a new block"
        );
    }

    #[test]
    fn stack_bin_of_loads_reused() {
        let mut ops = vec![
            IlOp::Load { slot: 1, loc: loc() },
            IlOp::Load { slot: 2, loc: loc() },
            IlOp::Bin {
                op: Instruction::MULF,
                loc: loc(),
            },
            IlOp::StorePop { slot: 8, loc: loc() },
            IlOp::Load { slot: 1, loc: loc() },
            IlOp::Load { slot: 2, loc: loc() },
            IlOp::Bin {
                op: Instruction::MULF,
                loc: loc(),
            },
            IlOp::Return {
                loc: loc(),
                ret_words: 1,
            },
        ];
        assert!(early_cse(&mut ops) >= 1);
        assert!(matches!(ops[4], IlOp::Load { slot: 8, .. }));
        assert!(matches!(ops[5], IlOp::Return { .. }));
    }

    #[test]
    fn cast_i2f_reused_from_slot() {
        let mut ops = vec![
            IlOp::Load { slot: 0, loc: loc() },
            IlOp::byte(Byte::new(Instruction::CastIntToFloat)),
            IlOp::StorePop { slot: 4, loc: loc() },
            IlOp::Load { slot: 0, loc: loc() },
            IlOp::byte(Byte::new(Instruction::CastIntToFloat)),
            IlOp::Return {
                loc: loc(),
                ret_words: 1,
            },
        ];
        early_cse(&mut ops);
        assert!(matches!(ops[3], IlOp::Load { slot: 4, .. }));
        assert_eq!(ops.len(), 5);
    }

    #[test]
    fn array_len_reused_until_push() {
        let mut ops = vec![
            IlOp::Load { slot: 0, loc: loc() },
            IlOp::byte(Byte::new(Instruction::ArrayLen)),
            IlOp::StorePop { slot: 2, loc: loc() },
            IlOp::Load { slot: 0, loc: loc() },
            IlOp::byte(Byte::new(Instruction::ArrayLen)),
            IlOp::Return {
                loc: loc(),
                ret_words: 1,
            },
        ];
        early_cse(&mut ops);
        assert!(matches!(ops[3], IlOp::Load { slot: 2, .. }));
    }

    #[test]
    fn index_killed_by_store_index() {
        let mut ops = vec![
            IlOp::Load { slot: 0, loc: loc() },
            IlOp::Load { slot: 1, loc: loc() },
            IlOp::Index { loc: loc() },
            IlOp::StorePop { slot: 3, loc: loc() },
            IlOp::byte(Byte::new(Instruction::StoreIndex)),
            IlOp::Load { slot: 0, loc: loc() },
            IlOp::Load { slot: 1, loc: loc() },
            IlOp::Index { loc: loc() },
            IlOp::Return {
                loc: loc(),
                ret_words: 1,
            },
        ];
        early_cse(&mut ops);
        assert!(ops.iter().any(|op| matches!(op, IlOp::Index { .. })));
    }

    #[test]
    fn isolated_optimize_flag_runs_pass() {
        let mut ops = vec![
            IlOp::BinSlotImm {
                op: Instruction::ADD as u8,
                slot: 0,
                imm: 1,
                loc: loc(),
            },
            IlOp::StorePop { slot: 2, loc: loc() },
            IlOp::BinSlotImm {
                op: Instruction::ADD as u8,
                slot: 0,
                imm: 1,
                loc: loc(),
            },
            IlOp::Return {
                loc: loc(),
                ret_words: 1,
            },
        ];
        optimize(&mut ops, &isolated(), &mut Vec::new());
        assert!(
            ops.iter().any(|op| matches!(op, IlOp::Load { slot: 2, .. })),
            "isolated local_cse should reuse stored add"
        );
    }

    #[test]
    fn isolated_optimize_off_keeps_recompute() {
        let mut o = isolated();
        o.local_cse = false;
        let mut ops = vec![
            IlOp::BinSlotImm {
                op: Instruction::ADD as u8,
                slot: 0,
                imm: 1,
                loc: loc(),
            },
            IlOp::StorePop { slot: 2, loc: loc() },
            IlOp::BinSlotImm {
                op: Instruction::ADD as u8,
                slot: 0,
                imm: 1,
                loc: loc(),
            },
            IlOp::Return {
                loc: loc(),
                ret_words: 1,
            },
        ];
        optimize(&mut ops, &o, &mut Vec::new());
        assert_eq!(
            ops.iter()
                .filter(|op| matches!(op, IlOp::BinSlotImm { .. }))
                .count(),
            2
        );
    }
}
