//! S2b — encode S2a live roots as interpreter slot / frame maps.
//!
//! Production still emits fuse-IL for allocating bodies. Maps attach when
//! IL→MIR with `allow_alloc` succeeds; dense / LIR emit stay refused.

use std::collections::BTreeSet;

use common::{Byte, FrameStackMap, Instruction, SlotMap};

use crate::il::IlOp;

use super::func::MirFunc;
use super::gc::{fill_live_roots, refuses_alloc};
use super::infer::infer_stack_map;
use super::inst::{MirInst, ValueId};
use super::lower::{try_lower_numeric, LowerHints};
use super::ty::MirTy;

/// Draft map keyed by function name until finalize assigns PCs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftFrameMap {
    pub name: String,
    /// Live heap slots at the n-th alloc site (Make* / InitTyped order).
    pub sites: Vec<Vec<u16>>,
}

/// Union of Alloc + following GcBarrier slots at each alloc site.
pub fn encode_draft(func: &MirFunc) -> Option<DraftFrameMap> {
    if func.gc_roots.is_empty() {
        return None;
    }
    let mut sites = Vec::new();
    for block in &func.blocks {
        let mut pending_alloc: Option<ValueId> = None;
        for inst in &block.insts {
            match inst {
                MirInst::Alloc { dest, .. } => pending_alloc = Some(*dest),
                MirInst::GcBarrier { dest, .. } => {
                    let mut slots = BTreeSet::new();
                    if let Some(a) = pending_alloc.take() {
                        take_slots(func, a, &mut slots);
                    }
                    take_slots(func, *dest, &mut slots);
                    sites.push(slots.into_iter().collect());
                }
                _ => pending_alloc = None,
            }
        }
    }
    if sites.is_empty() {
        return None;
    }
    Some(DraftFrameMap {
        name: func.name.clone(),
        sites,
    })
}

fn take_slots(func: &MirFunc, at: ValueId, into: &mut BTreeSet<u16>) {
    if let Some(side) = func.live_roots_at(at) {
        for s in &side.slots {
            if let Ok(u) = u16::try_from(s.0) {
                into.insert(u);
            }
        }
    }
}

/// Lift an allocating fuse-IL body for maps only (does not replace IL).
pub fn try_build_draft(
    ops: &[IlOp],
    name: &str,
    entry_sp: u32,
    pool: &[u64],
    unboxed_fields: &[(u32, u32)],
) -> Option<DraftFrameMap> {
    if !ops.iter().any(refuses_alloc) {
        return None;
    }
    let seed = seed_heap_params(ops, entry_sp);
    let inferred = infer_stack_map(ops, pool.len(), entry_sp, &seed).ok()?;
    let mut hints = LowerHints::new(name);
    hints.slot_ty = inferred.slot_ty;
    hints.pool = pool.to_vec();
    hints.pool_ty = inferred.pool_ty;
    hints.param_count = entry_sp;
    hints.allow_alloc = true;
    hints.allow_match = true;
    hints.allow_effects = true;
    hints.unboxed_fields = unboxed_fields.to_vec();
    hints.allow_fields = !unboxed_fields.is_empty();
    let mut func = try_lower_numeric(ops, &hints).ok()?;
    if func.gc_roots.is_empty() && func.has_gc_edge() {
        fill_live_roots(&mut func);
    }
    let mut draft = encode_draft(&func)?;
    draft.name = name.to_string();
    Some(draft)
}

/// Params that never see integer arith / INC are likely heap (`keep(xs)`).
fn seed_heap_params(ops: &[IlOp], entry_sp: u32) -> std::collections::HashMap<u32, MirTy> {
    let mut intish = BTreeSet::new();
    for op in ops {
        match op {
            IlOp::BinSlotImm { slot, .. } | IlOp::BinSlotSlot { a: slot, .. } => {
                intish.insert(u32::from(*slot));
            }
            IlOp::Byte { byte, .. }
                if matches!(
                    *byte.bytecode(),
                    Instruction::INC | Instruction::DEC
                ) =>
            {
                intish.insert(byte.inc_dec_parts().0 as u32);
            }
            _ => {}
        }
    }
    let mut seed = std::collections::HashMap::new();
    for s in 0..entry_sp {
        if !intish.contains(&s) {
            seed.insert(s, MirTy::HeapRef);
        }
    }
    seed
}

/// Bytecode opcodes that are interpreter GC safepoints (alloc).
pub fn is_alloc_opcode(inst: Instruction) -> bool {
    matches!(
        inst,
        Instruction::MakeArray
            | Instruction::MakeTuple
            | Instruction::MakeEnum
            | Instruction::InitTyped
            | Instruction::INIT
    )
}

/// Bind draft sites to finalized bytecode PCs.
pub fn bind_drafts(
    drafts: &[DraftFrameMap],
    bytecode: &[Byte],
    entries: &[(String, u32)],
) -> Vec<FrameStackMap> {
    if drafts.is_empty() || entries.is_empty() {
        return Vec::new();
    }
    let mut sorted = entries.to_vec();
    sorted.sort_by_key(|(_, pc)| *pc);
    let mut out = Vec::new();
    for (i, (name, entry_pc)) in sorted.iter().enumerate() {
        let Some(draft) = drafts.iter().find(|d| d.name == *name) else {
            continue;
        };
        let end_pc = sorted
            .get(i + 1)
            .map(|(_, p)| *p)
            .unwrap_or(bytecode.len() as u32);
        let start = *entry_pc as usize;
        let end = (end_pc as usize).min(bytecode.len());
        let mut alloc_pcs = Vec::new();
        for (off, b) in bytecode[start..end].iter().enumerate() {
            if is_alloc_opcode(*b.bytecode()) {
                alloc_pcs.push((start + off) as u32);
            }
        }
        if alloc_pcs.len() != draft.sites.len() {
            continue;
        }
        let mut frame: BTreeSet<u16> = BTreeSet::new();
        let mut safepoints = Vec::with_capacity(draft.sites.len());
        for (pc, slots) in alloc_pcs.into_iter().zip(draft.sites.iter()) {
            for s in slots {
                frame.insert(*s);
            }
            safepoints.push(SlotMap {
                pc,
                slots: slots.clone(),
            });
        }
        out.push(FrameStackMap {
            entry_pc: *entry_pc,
            end_pc,
            frame_slots: frame.into_iter().collect(),
            safepoints,
        });
    }
    out.sort_by_key(|m| m.entry_pc);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::il::{IlOp, Label};
    use crate::mir::builder::MirBuilder;
    use crate::mir::inst::{LocalId, MirAllocKind, MirCmpOp, MirConst, MirGcKind};
    use common::DebugLoc;

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

    #[test]
    fn draft_keeps_live_heap_slot() {
        let mut b = MirBuilder::new("keep");
        let xs = b.add_param(MirTy::HeapRef).unwrap();
        b.def_local(LocalId(0), xs).unwrap();
        let n = b.ins_const(MirConst::I64(1)).unwrap();
        let a = b.ins_alloc(MirAllocKind::Array, vec![n]).unwrap();
        let g = b.ins_gc_barrier(MirGcKind::Safepoint, vec![a]).unwrap();
        let _ = b.ins_cmp(MirCmpOp::Eq, xs, a).unwrap();
        b.ret(Some(g)).unwrap();
        let f = b.finish().unwrap();
        let draft = encode_draft(&f).expect("draft");
        assert_eq!(draft.sites.len(), 1);
        assert!(
            draft.sites[0].contains(&0),
            "slot 0: {:?}",
            draft.sites[0]
        );
    }

    #[test]
    fn try_build_draft_from_il_keep_slot() {
        let loc = loc();
        let ops = vec![
            IlOp::Label(Label(0)),
            IlOp::Const { imm: 1, loc },
            IlOp::MakeArray { arity: 1, loc },
            IlOp::StorePop { slot: 1, loc },
            IlOp::Load { slot: 0, loc },
            IlOp::Return { loc, ret_words: 1 },
        ];
        let draft = try_build_draft(&ops, "keep", 1, &[], &[]).expect("map");
        assert_eq!(draft.name, "keep");
        assert_eq!(draft.sites.len(), 1);
        assert!(
            draft.sites[0].contains(&0),
            "live param slot: {:?}",
            draft.sites[0]
        );
    }

    #[test]
    fn bind_pairs_alloc_pc() {
        let draft = DraftFrameMap {
            name: "keep".into(),
            sites: vec![vec![0]],
        };
        let bytecode = vec![
            Byte::new(Instruction::CONST).with_const_inline(1),
            Byte::new(Instruction::MakeArray).with_operand_u32(1),
            Byte::new(Instruction::RETURN),
        ];
        let maps = bind_drafts(&[draft], &bytecode, &[("keep".into(), 0)]);
        assert_eq!(maps.len(), 1);
        assert_eq!(maps[0].safepoints[0].pc, 1);
        assert_eq!(maps[0].safepoints[0].slots, vec![0]);
        assert_eq!(maps[0].frame_slots, vec![0]);
    }

    #[test]
    fn numeric_body_has_no_draft() {
        let loc = loc();
        let ops = vec![
            IlOp::Label(Label(0)),
            IlOp::Const { imm: 1, loc },
            IlOp::Return { loc, ret_words: 1 },
        ];
        assert!(try_build_draft(&ops, "n", 0, &[], &[]).is_none());
    }
}
