//! S2b — encode S2a live roots as interpreter slot / frame maps.
//!
//! S2c may specialize / MIR→LIR across alloc only when
//! [`try_build_draft`] succeeds (real maps). Unmapped alloc stays fuse-IL.

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
    /// Frame-wide heap slots (params + union of sites).
    pub frame_slots: Vec<u16>,
}

/// Union of Alloc + following GcBarrier slots at each alloc site.
pub fn encode_draft(func: &MirFunc) -> Option<DraftFrameMap> {
    if func.gc_roots.is_empty() {
        return None;
    }
    let mut sites: Vec<Vec<u16>> = Vec::new();
    for block in &func.blocks {
        let mut pending_alloc: Option<ValueId> = None;
        for inst in &block.insts {
            match inst {
                MirInst::Alloc { dest, .. }
                | MirInst::ArrayPush { dest, .. }
                | MirInst::Format { dest, .. }
                | MirInst::Stringify { dest, .. } => {
                    pending_alloc = Some(*dest)
                }
                MirInst::GcBarrier { dest, .. } => {
                    let mut slots: BTreeSet<u16> = BTreeSet::new();
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
    let mut frame: BTreeSet<u16> = BTreeSet::new();
    for site in &sites {
        for s in site {
            frame.insert(*s);
        }
    }
    Some(DraftFrameMap {
        name: func.name.clone(),
        sites,
        frame_slots: frame.into_iter().collect(),
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

/// True when an allocating body encodes a real S2b draft (one or more sites).
pub fn has_real_maps(
    ops: &[IlOp],
    name: &str,
    entry_sp: u32,
    pool: &[u64],
    unboxed_fields: &[(u32, u32)],
) -> bool {
    try_build_draft(ops, name, entry_sp, pool, unboxed_fields)
        .is_some_and(|d| !d.sites.is_empty())
}

/// Lift an allocating body for maps (and S2c specialize / LIR eligibility).
pub fn try_build_draft(
    ops: &[IlOp],
    name: &str,
    entry_sp: u32,
    pool: &[u64],
    unboxed_fields: &[(u32, u32)],
) -> Option<DraftFrameMap> {
    try_build_draft_err(ops, name, entry_sp, pool, unboxed_fields).ok()
}

fn try_build_draft_err(
    ops: &[IlOp],
    name: &str,
    entry_sp: u32,
    pool: &[u64],
    unboxed_fields: &[(u32, u32)],
) -> Result<DraftFrameMap, String> {
    if !ops.iter().any(refuses_alloc) {
        return Err("no alloc".into());
    }
    let inferred = infer_stack_map(ops, pool.len(), entry_sp, &Default::default())
        .map_err(|e| e.to_string())?;
    let mut hints = LowerHints::new(name);
    // Keep inferred param types (i64 `n` / index). Do not force HeapRef —
    // that broke looping `i < n` and `xs[k]` (S2d).
    hints.slot_ty = inferred.slot_ty;
    hints.pool = pool.to_vec();
    hints.pool_ty = inferred.pool_ty;
    hints.param_count = entry_sp;
    hints.allow_alloc = true;
    hints.allow_index = true;
    hints.allow_match = true;
    hints.allow_effects = true;
    hints.allow_string = true;
    hints.unboxed_fields = unboxed_fields.to_vec();
    hints.allow_fields = !unboxed_fields.is_empty();
    hints.allow_heap_fields = true;
    hints.skip_verify = true;
    let mut func = try_lower_numeric(ops, &hints).map_err(|e| e.to_string())?;
    if func.gc_roots.is_empty() && func.has_gc_edge() {
        fill_live_roots(&mut func);
    }
    let mut draft = encode_draft(&func).ok_or_else(|| "empty draft".to_string())?;
    draft.name = name.to_string();
    let mut frame: BTreeSet<u16> = draft.frame_slots.iter().copied().collect();
    for i in 0..entry_sp {
        if let Ok(u) = u16::try_from(i) {
            frame.insert(u);
        }
    }
    draft.frame_slots = frame.into_iter().collect();
    if draft.sites.iter().all(|s| s.is_empty()) {
        for site in &mut draft.sites {
            *site = draft.frame_slots.clone();
        }
    }
    Ok(draft)
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
            | Instruction::ArrayPush
            | Instruction::DenseMake
            | Instruction::DenseArrayPush
            | Instruction::FORMAT
            | Instruction::STRINGIFY
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
        let mut frame: BTreeSet<u16> = draft.frame_slots.iter().copied().collect();
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
            frame_slots: vec![0],
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
    fn try_build_draft_from_array_push() {
        let loc = loc();
        let ops = vec![
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 0, loc },
            IlOp::Const { imm: 1, loc },
            IlOp::byte(Byte::new(Instruction::ArrayPush)),
            IlOp::StorePop { slot: 0, loc },
            IlOp::Load { slot: 0, loc },
            IlOp::Return { loc, ret_words: 1 },
        ];
        let draft = try_build_draft(&ops, "grow", 1, &[], &[]).expect("map");
        assert_eq!(draft.sites.len(), 1);
        assert!(
            draft.sites[0].contains(&0),
            "live vec slot: {:?}",
            draft.sites[0]
        );
    }

    #[test]
    fn try_build_draft_from_init_typed_setfield() {
        let loc = loc();
        let ops = vec![
            IlOp::Label(Label(0)),
            IlOp::byte(
                Byte::new(Instruction::InitTyped)
                    .with_operand_u32(common::pack_init_typed(1, 2)),
            ),
            IlOp::StorePop { slot: 0, loc },
            IlOp::Const { imm: 3, loc },
            IlOp::Load { slot: 0, loc },
            IlOp::SetField {
                index: Some(0),
                loc,
            },
            IlOp::Pop { loc },
            IlOp::Const { imm: 4, loc },
            IlOp::Load { slot: 0, loc },
            IlOp::SetField {
                index: Some(1),
                loc,
            },
            IlOp::Pop { loc },
            IlOp::Load { slot: 0, loc },
            IlOp::Return { loc, ret_words: 1 },
        ];
        let draft = try_build_draft(&ops, "ctor", 0, &[], &[]).expect("D1 maps");
        assert_eq!(draft.sites.len(), 1);
        assert!(
            draft.sites[0].contains(&0),
            "InitTyped object slot: {:?}",
            draft.sites[0]
        );
    }

    #[test]
    fn try_build_draft_from_init_typed_getfield() {
        let loc = loc();
        let ops = vec![
            IlOp::Label(Label(0)),
            IlOp::byte(
                Byte::new(Instruction::InitTyped)
                    .with_operand_u32(common::pack_init_typed(1, 1)),
            ),
            IlOp::StorePop { slot: 0, loc },
            IlOp::Load { slot: 0, loc },
            IlOp::String { idx: 0, loc },
            IlOp::GetField { loc },
            IlOp::StorePop { slot: 1, loc },
            IlOp::Load { slot: 0, loc },
            IlOp::Return { loc, ret_words: 1 },
        ];
        let draft = try_build_draft(&ops, "get", 0, &[], &[]).expect("D1 maps");
        assert_eq!(draft.sites.len(), 1);
        assert!(
            draft.sites[0].contains(&0),
            "live object across GetField: {:?}",
            draft.sites[0]
        );
    }

    #[test]
    fn bind_pairs_dense_make_and_array_push() {
        let draft = DraftFrameMap {
            name: "grow".into(),
            sites: vec![vec![0], vec![0]],
            frame_slots: vec![0],
        };
        let bytecode = vec![
            Byte::new(Instruction::DenseMake).with_dense_abc(0, 1, 1, 2),
            Byte::new(Instruction::DenseArrayPush).with_dense_abc(0, 0, 0, 1),
        ];
        let maps = bind_drafts(&[draft], &bytecode, &[("grow".into(), 0)]);
        assert_eq!(maps.len(), 1);
        assert_eq!(maps[0].safepoints.len(), 2);
        assert_eq!(maps[0].safepoints[0].pc, 0);
        assert_eq!(maps[0].safepoints[1].pc, 1);
    }

    #[test]
    fn try_build_draft_from_format_keeps_live_slot() {
        let loc = loc();
        let ops = vec![
            IlOp::Label(Label(0)),
            IlOp::String { idx: 0, loc },
            IlOp::Load { slot: 0, loc },
            IlOp::byte(Byte::new(Instruction::FORMAT).with_operand_u32(1)),
            IlOp::StorePop { slot: 1, loc },
            IlOp::Load { slot: 0, loc },
            IlOp::Return { loc, ret_words: 1 },
        ];
        let draft = try_build_draft(&ops, "fmt", 1, &[], &[]).expect("map");
        assert_eq!(draft.sites.len(), 1);
        assert!(
            draft.sites[0].contains(&0),
            "live string slot: {:?}",
            draft.sites[0]
        );
    }

    #[test]
    fn bind_pairs_format_pc() {
        let draft = DraftFrameMap {
            name: "fmt".into(),
            sites: vec![vec![0]],
            frame_slots: vec![0],
        };
        let bytecode = vec![
            Byte::new(Instruction::STRING).with_operand_u32(0),
            Byte::new(Instruction::FORMAT).with_operand_u32(1),
            Byte::new(Instruction::RETURN),
        ];
        let maps = bind_drafts(&[draft], &bytecode, &[("fmt".into(), 0)]);
        assert_eq!(maps.len(), 1);
        assert_eq!(maps[0].safepoints[0].pc, 1);
        assert_eq!(maps[0].safepoints[0].slots, vec![0]);
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
