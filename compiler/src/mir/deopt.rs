//! I7 / C3 — debugger stop / deopt boundaries and resume maps.
//!
//! The VM debugger steps reconstructed bytecode (fuse-IL, LIR, or dense).
//! This sidecar names where a later native tier must pause or leave, and
//! records live IL slots at those edges. Production specialize does not set
//! [`crate::mir::LowerHints::allow_deopt`]. Explicit `Deopt` insts are
//! skipped at emit (not encoded). Maps stay compiler-internal — no archive
//! bump, no P5 resume.

use std::collections::{BTreeSet, HashMap};

use common::DebugLoc;

use crate::il::IlOp;

use super::effects::host_is_pure;
use super::func::{DeoptSite, MirFunc};
use super::gc::is_alloc_inst;
use super::inst::{LocalId, MirDeoptKind, MirInst, Terminator, ValueId};

impl MirInst {
    /// Kind if this inst is a leave-or-stop boundary (explicit or implicit).
    pub fn deopt_kind(&self) -> Option<MirDeoptKind> {
        match self {
            Self::Deopt { kind, .. } => Some(*kind),
            Self::Call { .. }
            | Self::Alloc { .. }
            | Self::GcBarrier { .. }
            | Self::Print { .. }
            | Self::Format { .. }
            | Self::Stringify { .. } => Some(MirDeoptKind::Deopt),
            Self::HostInvoke { native_id, .. } if !host_is_pure(*native_id) => {
                Some(MirDeoptKind::Deopt)
            }
            _ => None,
        }
    }
}

/// IL ops that become an explicit [`MirInst::Deopt`] when `allow_deopt`.
pub fn boundary_for_op(op: &IlOp) -> Option<MirDeoptKind> {
    match op {
        IlOp::HostInvoke { .. }
        | IlOp::Entry { .. }
        | IlOp::MakeArray { .. }
        | IlOp::MakeTuple { .. }
        | IlOp::MakeEnum { .. }
        | IlOp::Print { .. } => Some(MirDeoptKind::Deopt),
        IlOp::Byte { byte, .. }
            if is_alloc_inst(*byte.bytecode())
                || matches!(
                    *byte.bytecode(),
                    common::Instruction::FORMAT | common::Instruction::STRINGIFY
                ) =>
        {
            Some(MirDeoptKind::Deopt)
        }
        IlOp::Return { .. } | IlOp::Halt { .. } | IlOp::Jump { .. } => Some(MirDeoptKind::Stop),
        IlOp::StorePop { loc, .. } if loc.is_known() => Some(MirDeoptKind::Stop),
        _ => None,
    }
}

/// Compiler-internal resume map (not written to `.hyc`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftDeoptMap {
    pub name: String,
    pub sites: Vec<DraftDeoptSite>,
    /// False when a live SSA local has no reconstruct slot (stack-only / convoy).
    pub complete: bool,
}

/// One encoded leave edge: IL slots plus assigned reconstruct regs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftDeoptSite {
    pub kind: MirDeoptKind,
    pub loc: DebugLoc,
    pub il_slots: Vec<u16>,
    pub regs: Vec<u16>,
}

/// Fill [`MirFunc::deopt_sites`] from implicit leave edges and explicit `Deopt`.
///
/// Over-approximates live locals (safe). Under-approx would be unsound.
pub fn fill_deopt_maps(func: &mut MirFunc) {
    func.deopt_sites.clear();
    let fallback: Vec<(LocalId, ValueId)> = {
        let mut v: Vec<(LocalId, ValueId)> = func.debug_slots.iter().map(|(&l, &s)| (l, s)).collect();
        v.sort_by_key(|(l, _)| l.0);
        v
    };
    for block in &func.blocks {
        for inst in &block.insts {
            let Some(kind) = inst.deopt_kind() else {
                continue;
            };
            let at = inst.dest();
            let loc = match inst {
                MirInst::Deopt { loc, .. } => *loc,
                _ => func.loc_of(at),
            };
            func.deopt_sites
                .push(site_from(func, Some(at), kind, loc, &fallback));
        }
        match &block.term {
            Some(Terminator::Return { .. }) | Some(Terminator::Jump { .. }) => {
                let already = func.deopt_sites.iter().any(|s| {
                    s.kind == MirDeoptKind::Stop
                        && block.insts.last().is_some_and(|i| s.at == Some(i.dest()))
                });
                if already {
                    continue;
                }
                let loc = func.term_loc(block.id);
                func.deopt_sites.push(site_from(
                    func,
                    None,
                    MirDeoptKind::Stop,
                    loc,
                    &fallback,
                ));
            }
            _ => {}
        }
    }
}

fn site_from(
    func: &MirFunc,
    at: Option<ValueId>,
    kind: MirDeoptKind,
    loc: DebugLoc,
    fallback: &[(LocalId, ValueId)],
) -> DeoptSite {
    let mut slots: BTreeSet<(u32, ValueId)> = BTreeSet::new();
    if let Some(at) = at {
        if let Some(env) = func.slot_env.get(&at) {
            for (l, v) in env {
                slots.insert((l.0, *v));
            }
        }
    }
    if slots.is_empty() {
        for (l, v) in fallback {
            slots.insert((l.0, *v));
        }
    }
    DeoptSite {
        kind,
        at,
        loc,
        slots: slots
            .into_iter()
            .map(|(id, v)| (LocalId(id), v))
            .collect(),
    }
}

/// Original IL slot → reconstruct register for named locals that have a slot.
pub fn debug_slot_remap(
    func: &MirFunc,
    regs: &[u8],
    need_slot: &[bool],
) -> HashMap<u32, u32> {
    let mut out = HashMap::new();
    for (local, val) in &func.debug_slots {
        let idx = val.index();
        let is_param = func.params.iter().any(|p| *p == *val);
        if !is_param && !need_slot.get(idx).copied().unwrap_or(false) {
            continue;
        }
        if let Some(&r) = regs.get(idx) {
            out.insert(local.0, u32::from(r));
        }
    }
    out
}

/// Encode leave edges after register assign. Incomplete maps must not resume.
pub fn encode_draft(func: &MirFunc, regs: &[u8], need_slot: &[bool]) -> DraftDeoptMap {
    let mut sites = Vec::new();
    let mut complete = true;
    for site in &func.deopt_sites {
        let mut il_slots = Vec::new();
        let mut mapped = Vec::new();
        for (local, val) in &site.slots {
            let idx = val.index();
            let is_param = func.params.iter().any(|p| *p == *val);
            let slotted = is_param || need_slot.get(idx).copied().unwrap_or(false);
            if let Ok(il) = u16::try_from(local.0) {
                il_slots.push(il);
            }
            if slotted {
                if let Some(&r) = regs.get(idx) {
                    mapped.push(u16::from(r));
                    continue;
                }
            }
            complete = false;
        }
        sites.push(DraftDeoptSite {
            kind: site.kind,
            loc: site.loc,
            il_slots,
            regs: mapped,
        });
    }
    DraftDeoptMap {
        name: func.name.clone(),
        sites,
        complete,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::il::Label;
    use common::DebugLoc;
    use super::super::inst::ValueId;

    fn loc() -> DebugLoc {
        DebugLoc {
            file: 0,
            start_byte: 0,
            end_byte: 4,
        }
    }

    #[test]
    fn implicit_call_and_explicit_stop() {
        let call = MirInst::Call {
            dest: ValueId(0),
            dest_hi: None,
            target: Label(1),
            args: Vec::new(),
        };
        assert_eq!(call.deopt_kind(), Some(MirDeoptKind::Deopt));
        let stop = MirInst::Deopt {
            dest: ValueId(1),
            kind: MirDeoptKind::Stop,
            loc: loc(),
        };
        assert!(stop.is_deopt_edge());
        assert_eq!(stop.deopt_kind(), Some(MirDeoptKind::Stop));
        assert!(
            boundary_for_op(&IlOp::Return {
                loc: loc(),
                ret_words: 1
            })
            .is_some()
        );
        assert!(
            boundary_for_op(&IlOp::Const {
                imm: 1,
                loc: loc()
            })
            .is_none()
        );
    }

    #[test]
    fn encode_draft_maps_param_slot() {
        let mut func = MirFunc::new("hot");
        func.params.push(ValueId(0));
        func.types.push(super::super::ty::MirTy::I64);
        func.debug_slots.insert(LocalId(0), ValueId(0));
        func.deopt_sites.push(DeoptSite {
            kind: MirDeoptKind::Stop,
            at: None,
            loc: loc(),
            slots: vec![(LocalId(0), ValueId(0))],
        });
        let regs = vec![0u8];
        let need = vec![true];
        let draft = encode_draft(&func, &regs, &need);
        assert!(draft.complete);
        assert_eq!(draft.sites.len(), 1);
        assert_eq!(draft.sites[0].il_slots, vec![0]);
        assert_eq!(draft.sites[0].regs, vec![0]);
        let remap = debug_slot_remap(&func, &regs, &need);
        assert_eq!(remap.get(&0), Some(&0));
    }
}
