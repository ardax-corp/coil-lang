//! S2b — encode S2a live roots as interpreter slot / frame maps.
//!
//! Dense and LIR bodies built from MIR (lowered from HIR) carry a map in the
//! frame registers their emission assigns ([`encode_draft_regs`]). Fuse-IL
//! bodies carry none: their precise frame maps cover them.

use std::collections::BTreeSet;

use common::{Byte, FrameStackMap, Instruction, SlotMap};

use super::func::MirFunc;
use super::gc::fill_live_roots;
use super::inst::MirInst;

/// Draft map keyed by function name until finalize assigns PCs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftFrameMap {
    pub name: String,
    /// Live heap slots at the n-th alloc site (Make* / InitTyped order).
    /// Empty: `frame_slots` hold at every PC of the body.
    pub sites: Vec<Vec<u16>>,
    /// Frame-wide heap slots (params + union of sites).
    pub frame_slots: Vec<u16>,
}

/// A map built from MIR itself, for a body whose MIR did not come from
/// lifting its stack IL (HIR to MIR): one site per allocation, each the
/// frame registers (`regs`, from LIR emission) of every heap value live at
/// any site, plus the parameters. Emission may place a tree allocation
/// where it is used, so per-site sets would not follow the bytecode order;
/// the union does, and a slot holding a non-heap word there is skipped by
/// the collector. `None` when the body has no GC edge.
pub fn encode_draft_regs(func: &MirFunc, regs: &[u8], need_slot: &[bool]) -> Option<DraftFrameMap> {
    let mut func = func.clone();
    fill_live_roots(&mut func);
    if func.gc_roots.is_empty() {
        return None;
    }
    let mut slots: BTreeSet<u16> = (0..func.params.len()).filter_map(|i| u16::try_from(i).ok()).collect();
    for set in &func.gc_roots {
        for v in &set.values {
            let slotted = func.params.contains(v) || need_slot.get(v.index()).copied().unwrap_or(false);
            if slotted && let Some(&r) = regs.get(v.index()) {
                slots.insert(u16::from(r));
            }
        }
    }
    // One site per allocating instruction: each emits one alloc opcode.
    let n = func
        .blocks
        .iter()
        .flat_map(|b| &b.insts)
        .filter(|i| matches!(i, MirInst::Alloc { .. } | MirInst::ArrayPush { .. } | MirInst::Format { .. } | MirInst::Stringify { .. }))
        .count();
    if n == 0 {
        return None;
    }
    let slots: Vec<u16> = slots.into_iter().collect();
    Some(DraftFrameMap {
        name: func.name.clone(),
        sites: vec![slots.clone(); n],
        frame_slots: slots,
    })
}

/// The map of a fuse-IL body emitted from the same HIR as `func` (MIR
/// lowered from HIR): its frame slots are `func`'s locals, so every local a
/// heap word was stored in (a heap parameter included) holds at every PC. A
/// slot that also holds a non-heap word at some point is left to the
/// precise frame map, so no scalar is read as a handle. A body that keeps
/// no heap word in a slot gets an empty map.
pub fn encode_draft_slots(func: &MirFunc) -> DraftFrameMap {
    let frame_slots = func
        .heap_slots
        .difference(&func.word_slots)
        .filter_map(|l| u16::try_from(l.0).ok())
        .collect();
    DraftFrameMap {
        name: func.name.clone(),
        sites: Vec::new(),
        frame_slots,
    }
}

/// Whether direct MIR can give its allocating body maps: every allocation
/// is followed by its `GcBarrier`.
pub fn mir_maps_ok(func: &MirFunc) -> bool {
    let mut any = false;
    for block in &func.blocks {
        let mut pending = false;
        for inst in &block.insts {
            match inst {
                MirInst::Alloc { .. } | MirInst::ArrayPush { .. } | MirInst::Format { .. } | MirInst::Stringify { .. } => {
                    if pending {
                        return false;
                    }
                    pending = true;
                }
                MirInst::GcBarrier { .. } => {
                    pending = false;
                    any = true;
                }
                _ if pending => return false,
                _ => {}
            }
        }
        if pending {
            return false;
        }
    }
    any
}

/// Bytecode opcodes that are interpreter GC safepoints (alloc).
pub fn is_alloc_opcode(inst: Instruction) -> bool {
    matches!(
        inst,
        Instruction::MakeArray
            | Instruction::MakeArrayK
            | Instruction::MakeTuple
            | Instruction::MakeTupleK
            | Instruction::MakeEnum
            | Instruction::MakeEnumK
            | Instruction::MakeEnumReturn
            | Instruction::MakeEnumReturnK
            | Instruction::InitTyped
            | Instruction::INIT
            | Instruction::ArrayPush
            | Instruction::DenseMake
            | Instruction::DenseMakeK
            | Instruction::DenseMakeObject
            | Instruction::DenseArrayPush
            | Instruction::FORMAT
            | Instruction::STRINGIFY
            |             Instruction::DictEntries
            | Instruction::MakeDict
            | Instruction::MakeCoro
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
        if draft.sites.is_empty() {
            out.push(FrameStackMap {
                entry_pc: *entry_pc,
                end_pc,
                frame_slots: draft.frame_slots.clone(),
                safepoints: Vec::new(),
            });
            continue;
        }
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
    fn slot_draft_keeps_heap_locals_only() {
        // Slot 0: a heap param. Slot 1: an int. Slot 2: an array, later an
        // int, so the precise frame map keeps it instead.
        let mut b = MirBuilder::new("slots");
        let xs = b.add_param(MirTy::HeapRef).unwrap();
        b.def_local(LocalId(0), xs).unwrap();
        let n = b.ins_const(MirConst::I64(1)).unwrap();
        b.def_local(LocalId(1), n).unwrap();
        let a = b.ins_alloc(MirAllocKind::Array, vec![n]).unwrap();
        let g = b.ins_gc_barrier(MirGcKind::Safepoint, vec![a]).unwrap();
        b.def_local(LocalId(2), g).unwrap();
        b.def_local(LocalId(2), n).unwrap();
        b.ret(Some(xs)).unwrap();
        let f = b.finish().unwrap();
        let draft = encode_draft_slots(&f);
        assert!(draft.sites.is_empty());
        assert_eq!(draft.frame_slots, vec![0]);
    }

    #[test]
    fn bind_slot_draft_holds_at_every_pc() {
        let draft = DraftFrameMap {
            name: "keep".into(),
            sites: Vec::new(),
            frame_slots: vec![0, 3],
        };
        let bytecode = vec![
            Byte::new(Instruction::CONST).with_const_inline(1),
            Byte::new(Instruction::MakeArray).with_operand_u32(1),
            Byte::new(Instruction::RETURN),
        ];
        let maps = bind_drafts(&[draft], &bytecode, &[("keep".into(), 0)]);
        assert_eq!(maps.len(), 1);
        assert!(maps[0].safepoints.is_empty());
        assert_eq!(maps[0].frame_slots, vec![0, 3]);
        for pc in 0..3 {
            assert_eq!(maps[0].slots_at(pc), &[0, 3]);
        }
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

}
