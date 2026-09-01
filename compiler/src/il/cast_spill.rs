//! Spill `CastIntToFloat` that blocks a float-arith→STORE fuse window.
//!
//! Mandelbrot `cr`/`ci` are typically `CONST; LOAD; Cast; …; STORE`. Inline
//! `Cast; STORE t; LOAD t` leaves glue between the const and stage0. This peep
//! hoists every `LOAD; Cast` in the window into a prefix of
//! `LOAD; Cast; STORE t`, then rewrites the body to `LOAD t` so fuse can match
//! const-under / `LOAD; CONST` `FloatChainStore` shapes.

use common::Instruction;

use super::op::IlOp;

/// Hoist casts that block a float-arith→STORE window into float temps.
pub fn spill_cast_before_float_chain(ops: &mut Vec<IlOp>) {
    if ops.len() < 4 {
        return;
    }
    let mut max_slot = max_slot_used(ops);
    loop {
        let mut spilled = false;
        let mut i = 0;
        while i < ops.len() {
            if !is_cast_int_to_float(&ops[i]) {
                i += 1;
                continue;
            }
            // Already a hoisted `Cast; STORE t` (LOAD of t appears in the body).
            if i + 1 < ops.len() && matches!(ops[i + 1], IlOp::StorePop { .. }) {
                i += 1;
                continue;
            }
            let Some(store_i) = float_chain_store_after(ops, i) else {
                i += 1;
                continue;
            };
            let Some(new_max) = hoist_casts_in_window(ops, i, store_i, max_slot) else {
                i += 1;
                continue;
            };
            max_slot = new_max;
            spilled = true;
            break;
        }
        if !spilled {
            break;
        }
    }
}

/// Rewrite `[window_start, store_i]` so all `LOAD; Cast` sites become a prefix
/// of spills and `LOAD temp` in the float body. `cast_i` is any cast in the window.
fn hoist_casts_in_window(
    ops: &mut Vec<IlOp>,
    cast_i: usize,
    store_i: usize,
    mut max_slot: u32,
) -> Option<u32> {
    let mut start = cast_i;
    if cast_i > 0 && is_load(&ops[cast_i - 1]) {
        start = cast_i - 1;
    }
    // Pull left through leading CONST/ConstPool so they stay with the float body
    // after the cast prefix (const-under stage0).
    while start > 0 && is_float_const(&ops[start - 1]) {
        start -= 1;
    }

    let mut sites: Vec<(usize, usize)> = Vec::new();
    let mut j = start;
    while j < store_i {
        if is_cast_int_to_float(&ops[j]) {
            if j > start && is_load(&ops[j - 1]) {
                if j + 1 < ops.len() && matches!(ops[j + 1], IlOp::StorePop { .. }) {
                    j += 1;
                    continue;
                }
                sites.push((j - 1, j));
                j += 1;
                continue;
            }
            return None;
        }
        j += 1;
    }
    if sites.is_empty() {
        return None;
    }

    let mut temps: Vec<u32> = Vec::with_capacity(sites.len());
    for _ in &sites {
        max_slot = max_slot.saturating_add(1);
        temps.push(max_slot);
    }

    let mut prefix: Vec<IlOp> = Vec::new();
    for (k, &(load_i, cast_idx)) in sites.iter().enumerate() {
        let loc = ops[cast_idx].loc();
        let slot = match &ops[load_i] {
            IlOp::Load { slot, .. } => *slot,
            _ => return None,
        };
        prefix.push(IlOp::Load { slot, loc });
        prefix.push(ops[cast_idx].clone());
        prefix.push(IlOp::StorePop {
            slot: temps[k],
            loc,
        });
    }

    let site_load: std::collections::HashSet<usize> = sites.iter().map(|(l, _)| *l).collect();
    let site_cast: std::collections::HashMap<usize, u32> = sites
        .iter()
        .enumerate()
        .map(|(k, &(_, c))| (c, temps[k]))
        .collect();

    let mut body: Vec<IlOp> = Vec::new();
    for idx in start..store_i {
        if site_load.contains(&idx) {
            continue;
        }
        if let Some(&temp) = site_cast.get(&idx) {
            body.push(IlOp::Load {
                slot: temp,
                loc: ops[idx].loc(),
            });
            continue;
        }
        body.push(ops[idx].clone());
    }
    body.push(ops[store_i].clone());

    let mut rebuilt = prefix;
    rebuilt.extend(body);
    ops.splice(start..=store_i, rebuilt);
    Some(max_slot)
}

fn float_chain_store_after(ops: &[IlOp], cast_i: usize) -> Option<usize> {
    let window_end = ops.len().min(cast_i + 1 + 12);
    let mut float_ops = 0usize;
    for (idx, op) in ops.iter().enumerate().take(window_end).skip(cast_i + 1) {
        if is_float_arith(op) {
            float_ops += 1;
        }
        if is_store(op) {
            return (float_ops >= 2).then_some(idx);
        }
        if matches!(
            op,
            IlOp::Jump { .. } | IlOp::Label(_) | IlOp::JoinLabel(_) | IlOp::Return { .. } | IlOp::Halt { .. }
        ) {
            return None;
        }
    }
    None
}

fn max_slot_used(ops: &[IlOp]) -> u32 {
    let mut m = 0u32;
    for op in ops {
        match op {
            IlOp::Load { slot, .. } | IlOp::StorePop { slot, .. } => m = m.max(*slot),
            IlOp::BinSlotImm { slot, .. } => m = m.max(*slot as u32),
            IlOp::BinSlotSlot { a, b, .. } => m = m.max(*a as u32).max(*b as u32),
            IlOp::Byte { byte, .. }
                if matches!(
                    *byte.bytecode(),
                    Instruction::LOAD | Instruction::STORE | Instruction::StorePop
                ) =>
            {
                for k in 0..byte.load_store_count() {
                    m = m.max(byte.load_store_slot_at(k));
                }
            }
            _ => {}
        }
    }
    m
}

fn is_load(op: &IlOp) -> bool {
    matches!(op, IlOp::Load { .. })
        || op
            .as_encode_byte()
            .is_some_and(|b| *b.bytecode() == Instruction::LOAD && b.load_store_count() == 1)
}

fn is_float_const(op: &IlOp) -> bool {
    matches!(op, IlOp::ConstPool { .. })
        || op.as_encode_byte().is_some_and(|b| {
            *b.bytecode() == Instruction::CONST && b.operand_u32() & common::Byte::POOL_FLAG != 0
        })
}

fn is_cast_int_to_float(op: &IlOp) -> bool {
    match op {
        IlOp::Byte { byte, .. } => *byte.bytecode() == Instruction::CastIntToFloat,
        other => other
            .as_encode_byte()
            .is_some_and(|b| *b.bytecode() == Instruction::CastIntToFloat),
    }
}

fn is_float_arith(op: &IlOp) -> bool {
    match op {
        IlOp::Bin { op, .. } => matches!(
            *op,
            Instruction::ADDF
                | Instruction::SUBF
                | Instruction::MULF
                | Instruction::DIVF
                | Instruction::MODF
        ),
        IlOp::BinSlotSlot { op, .. } | IlOp::BinSlotImm { op, .. } => matches!(
            Instruction::from(*op),
            Instruction::ADDF
                | Instruction::SUBF
                | Instruction::MULF
                | Instruction::DIVF
                | Instruction::MODF
        ),
        other => other.as_encode_byte().is_some_and(|b| {
            matches!(
                *b.bytecode(),
                Instruction::ADDF
                    | Instruction::SUBF
                    | Instruction::MULF
                    | Instruction::DIVF
                    | Instruction::MODF
            )
        }),
    }
}

fn is_store(op: &IlOp) -> bool {
    matches!(op, IlOp::StorePop { .. })
        || op.as_encode_byte().is_some_and(|b| {
            matches!(
                *b.bytecode(),
                Instruction::STORE | Instruction::StorePop | Instruction::FloatChainStore
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{Byte, DebugLoc};

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

    #[test]
    fn spills_cast_inside_float_arith_store_window() {
        let mut ops = vec![
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::byte(Byte::new(Instruction::CastIntToFloat)),
            IlOp::ConstPool {
                idx: 0,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::MULF,
                loc: loc(),
            },
            IlOp::ConstPool {
                idx: 1,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::SUBF,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 1,
                loc: loc(),
            },
        ];
        spill_cast_before_float_chain(&mut ops);
        assert!(matches!(ops[0], IlOp::Load { slot: 0, .. }));
        assert!(is_cast_int_to_float(&ops[1]));
        assert!(matches!(ops[2], IlOp::StorePop { .. }));
        assert!(matches!(ops[3], IlOp::Load { .. }));
    }

    #[test]
    fn spills_both_casts_in_two_cast_float_window() {
        let mut ops = vec![
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::byte(Byte::new(Instruction::CastIntToFloat)),
            IlOp::ConstPool {
                idx: 0,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::MULF,
                loc: loc(),
            },
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::byte(Byte::new(Instruction::CastIntToFloat)),
            IlOp::Bin {
                op: Instruction::DIVF,
                loc: loc(),
            },
            IlOp::ConstPool {
                idx: 1,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::SUBF,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 2,
                loc: loc(),
            },
        ];
        spill_cast_before_float_chain(&mut ops);
        let cast_store_pairs = ops
            .windows(2)
            .filter(|w| {
                is_cast_int_to_float(&w[0]) && matches!(w[1], IlOp::StorePop { .. })
            })
            .count();
        assert_eq!(cast_store_pairs, 2);
        let body_start = ops
            .windows(2)
            .enumerate()
            .filter(|(_, w)| {
                is_cast_int_to_float(&w[0]) && matches!(w[1], IlOp::StorePop { .. })
            })
            .map(|(i, _)| i + 2)
            .max()
            .unwrap();
        assert!(ops[body_start..ops.len() - 1]
            .iter()
            .all(|o| !is_cast_int_to_float(o)));
    }

    #[test]
    fn spills_const_under_mid_chain_cast() {
        let mut ops = vec![
            IlOp::ConstPool {
                idx: 0,
                loc: loc(),
            },
            IlOp::Load {
                slot: 4,
                loc: loc(),
            },
            IlOp::byte(Byte::new(Instruction::CastIntToFloat)),
            IlOp::Load {
                slot: 13,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::DIVF,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::MULF,
                loc: loc(),
            },
            IlOp::ConstPool {
                idx: 1,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::SUBF,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 5,
                loc: loc(),
            },
        ];
        spill_cast_before_float_chain(&mut ops);
        assert!(matches!(ops[0], IlOp::Load { slot: 4, .. }));
        assert!(is_cast_int_to_float(&ops[1]));
        assert!(matches!(ops[2], IlOp::StorePop { .. }));
        assert!(matches!(ops[3], IlOp::ConstPool { idx: 0, .. }));
        assert!(matches!(ops[4], IlOp::Load { .. }));
        assert!(ops[4..ops.len() - 1]
            .iter()
            .all(|o| !is_cast_int_to_float(o)));
    }

    /// Exact pre-opt mandelbrot `cr` IL (two casts, const-under).
    #[test]
    fn spills_mandelbrot_cr_two_cast_const_under() {
        let mut ops = vec![
            IlOp::ConstPool {
                idx: 0,
                loc: loc(),
            },
            IlOp::Load {
                slot: 4,
                loc: loc(),
            },
            IlOp::byte(Byte::new(Instruction::CastIntToFloat)),
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::byte(Byte::new(Instruction::CastIntToFloat)),
            IlOp::Bin {
                op: Instruction::DIVF,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::MULF,
                loc: loc(),
            },
            IlOp::ConstPool {
                idx: 1,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::SUBF,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 5,
                loc: loc(),
            },
        ];
        spill_cast_before_float_chain(&mut ops);
        assert_eq!(
            ops
                .windows(2)
                .filter(|w| {
                    is_cast_int_to_float(&w[0]) && matches!(w[1], IlOp::StorePop { .. })
                })
                .count(),
            2
        );
        assert!(matches!(ops[6], IlOp::ConstPool { idx: 0, .. }));
    }

    #[test]
    fn refuses_cast_without_float_chain_store_window() {
        let mut ops = vec![
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::byte(Byte::new(Instruction::CastIntToFloat)),
            IlOp::StorePop {
                slot: 1,
                loc: loc(),
            },
        ];
        let before = ops.clone();
        spill_cast_before_float_chain(&mut ops);
        assert!(ops == before);
    }

    #[test]
    fn refuses_when_jump_interrupts_float_window() {
        use super::super::op::{IlJumpKind, Label};
        let mut ops = vec![
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::byte(Byte::new(Instruction::CastIntToFloat)),
            IlOp::ConstPool {
                idx: 0,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::MULF,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(1),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
            IlOp::ConstPool {
                idx: 1,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::SUBF,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 1,
                loc: loc(),
            },
        ];
        let before = ops.clone();
        spill_cast_before_float_chain(&mut ops);
        assert!(ops == before);
    }

    #[test]
    fn optimize_with_cast_spill_disabled_keeps_inline_cast() {
        use super::super::opt::{OptimizeOptions, optimize};
        let mut ops = vec![
            IlOp::ConstPool {
                idx: 0,
                loc: loc(),
            },
            IlOp::Load {
                slot: 4,
                loc: loc(),
            },
            IlOp::byte(Byte::new(Instruction::CastIntToFloat)),
            IlOp::Load {
                slot: 13,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::DIVF,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::MULF,
                loc: loc(),
            },
            IlOp::ConstPool {
                idx: 1,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::SUBF,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 5,
                loc: loc(),
            },
            IlOp::Return { loc: loc() },
        ];
        optimize(
            &mut ops,
            &OptimizeOptions {
                cast_spill: false,
                canon: false,
                algebraic: false,
                licm: false,
                loop_bounds: false,
                slot_promote: false,
                tos_carry: false,
                mem_fwd: false,
                copy_prop: false,
                ..OptimizeOptions::default()
            },
            &mut Vec::new(),
        );
        // Cast stays between Load and float arith — no Cast;STORE spill prefix.
        assert!(is_cast_int_to_float(&ops[2]));
        assert!(!matches!(ops[3], IlOp::StorePop { .. }));
    }
}
