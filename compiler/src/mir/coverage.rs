//! Coverage probes in captured MIR.
//!
//! Each distinct source location of a body gets a [`MirInst::Probe`] in every
//! block that runs it, so line coverage counts on any engine that runs MIR. A
//! probe's id indexes the table [`instrument`] returns, which stands where a
//! code program's PC to location table stands for the runtime's counts.

use std::collections::HashSet;
use std::ops::Range;

use common::DebugLoc;

use super::{MirBlock, MirFunc, MirInst, MirTy, NativeBody, ValueId};

/// What [`instrument`] numbered.
#[derive(Debug, Default)]
pub struct Probes {
    /// Each probe's location.
    pub locs: Vec<DebugLoc>,
    /// Each body's probes, by name.
    pub bodies: Vec<(String, Range<usize>)>,
}

/// Add probes to `bodies`.
pub fn instrument(bodies: &mut [NativeBody]) -> Probes {
    let mut probes = Probes::default();
    for body in bodies {
        let start = probes.locs.len();
        instrument_body(&mut body.mir, &mut probes.locs);
        probes.bodies.push((body.name.clone(), start..probes.locs.len()));
    }
    probes
}

fn instrument_body(func: &mut MirFunc, locs: &mut Vec<DebugLoc>) {
    let mut blocks = std::mem::take(&mut func.blocks);
    for block in &mut blocks {
        instrument_block(func, block, locs);
    }
    func.blocks = blocks;
}

fn instrument_block(func: &mut MirFunc, block: &mut MirBlock, locs: &mut Vec<DebugLoc>) {
    let mut seen = HashSet::new();
    let insts = std::mem::take(&mut block.insts);
    let mut out = Vec::with_capacity(insts.len());
    for inst in insts {
        if !inst.is_phi() {
            let dest = inst.dest();
            let loc = func.op_locs.get(&dest).or_else(|| func.value_locs.get(&dest)).copied();
            add_probe(func, loc, &mut seen, &mut out, locs);
        }
        out.push(inst);
    }
    let term = func.term_locs.get(&block.id).copied();
    add_probe(func, term, &mut seen, &mut out, locs);
    block.insts = out;
}

fn add_probe(func: &mut MirFunc, loc: Option<DebugLoc>, seen: &mut HashSet<(u32, u32)>, out: &mut Vec<MirInst>, locs: &mut Vec<DebugLoc>) {
    let Some(loc) = loc.filter(|l| l.is_known()) else { return };
    if !seen.insert((loc.file, loc.start_byte)) {
        return;
    }
    let dest = ValueId(func.types.len() as u32);
    func.types.push(MirTy::Bool);
    out.push(MirInst::Probe { dest, id: locs.len() as u32 });
    locs.push(loc);
}
