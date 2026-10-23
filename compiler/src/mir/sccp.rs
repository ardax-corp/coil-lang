//! Sparse conditional constant propagation (Wegman–Zadeck).
//!
//! Every value starts unknown (⊤) and every block unreached, except the
//! entry. Reached blocks evaluate their instructions over the lattice
//! ⊤ → constant → varying (⊥): a φ meets only the values on edges that
//! run, and a branch on a known condition runs one edge. Values are
//! assumed constant until proven otherwise, so a loop φ that only ever
//! gets its own value back stays constant, and a join whose other arms
//! never run takes the one that does — what `instcombine` (which skips
//! φs) and `tidy` (which only drops blocks nothing jumps to) miss.
//!
//! Then each value proven constant becomes a `Const`, a branch on one
//! becomes a jump, φ edges from blocks that never run go, and the blocks
//! nothing reaches are dropped. An op that would trap is never constant
//! ([`super::instcombine::const_of`] does not fold it), so it stays and
//! traps at run time.

use std::collections::{HashMap, HashSet};

use super::func::MirFunc;
use super::inst::{BlockId, MirConst, MirInst, Terminator, ValueId};

/// Upper bound on sweeps; the lattice is three high, so a body settles
/// in a few unless it is huge, and then it is left as it was.
const MAX_SWEEPS: usize = 64;

#[derive(Copy, Clone, Debug, PartialEq)]
enum Lat {
    Top,
    Const(MirConst),
    Bottom,
}

fn meet(a: Lat, b: Lat) -> Lat {
    match (a, b) {
        (Lat::Top, x) | (x, Lat::Top) => x,
        (Lat::Const(x), Lat::Const(y)) if x == y => a,
        _ => Lat::Bottom,
    }
}

/// Fold what SCCP proves constant. Returns how many values and branches
/// it rewrote.
pub fn sccp(func: &mut MirFunc) -> usize {
    if !enabled() {
        return 0;
    }
    let Some((vals, reached, edges)) = solve(func) else {
        return 0;
    };
    let mut hits = 0;
    let mut subst: HashMap<ValueId, ValueId> = HashMap::new();
    for block in &mut func.blocks {
        if !reached.contains(&block.id) {
            continue;
        }
        let id = block.id;
        // φ edges from blocks that never run go.
        for inst in &mut block.insts {
            if let MirInst::Phi { args, .. } = inst {
                let n = args.len();
                args.retain(|(from, _)| edges.contains(&(*from, id)));
                hits += usize::from(args.len() != n);
            }
        }
        // Constant values become `Const`s (a φ's after the φs).
        let mut consts_after_phis = Vec::new();
        for inst in &mut block.insts {
            if matches!(inst, MirInst::Const { .. }) {
                continue;
            }
            let dests: Vec<ValueId> = inst.dests().collect();
            let [dest] = dests[..] else { continue };
            let Some(Lat::Const(c)) = vals.get(dest.index()).copied() else { continue };
            if !foldable(inst) {
                continue;
            }
            if inst.is_phi() {
                consts_after_phis.push(MirInst::Const { dest, c });
                // The φ itself goes below; mark it by emptying its args.
                if let MirInst::Phi { args, .. } = inst {
                    args.clear();
                }
            } else {
                *inst = MirInst::Const { dest, c };
            }
            hits += 1;
        }
        if !consts_after_phis.is_empty() {
            block.insts.retain(|i| !matches!(i, MirInst::Phi { args, .. } if args.is_empty()));
            let at = block.insts.iter().take_while(|i| i.is_phi()).count();
            block.insts.splice(at..at, consts_after_phis);
        }
        // A φ left with one incoming value is that value. Its uses are
        // rewritten below, so it counts as a fold (#1898).
        block.insts.retain(|i| match i {
            MirInst::Phi { dest, args, .. } if args.len() == 1 => {
                subst.insert(*dest, args[0].1);
                hits += 1;
                false
            }
            _ => true,
        });
        // A branch on a known condition runs one way.
        if let Some(Terminator::Br { cond, taken, not_taken }) = block.term
            && let Some(Lat::Const(MirConst::Bool(c))) = vals.get(cond.index()).copied()
        {
            block.term = Some(Terminator::Jump { dest: if c { taken } else { not_taken } });
            hits += 1;
        }
    }
    if hits == 0 {
        return 0;
    }
    let resolve = |mut v: ValueId| {
        while let Some(&n) = subst.get(&v) {
            v = n;
        }
        v
    };
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            inst.rewrite_values(resolve);
        }
        if let Some(term) = &mut block.term {
            term.rewrite_values(resolve);
        }
    }
    func.rewrite_debug_slots(resolve);
    // Blocks that never run lose their edges into blocks that do.
    for block in &mut func.blocks {
        if !reached.contains(&block.id) {
            block.insts.clear();
            block.term = Some(Terminator::Unreachable);
        }
    }
    let order: Vec<BlockId> = func.blocks.iter().map(|b| b.id).filter(|b| reached.contains(b)).collect();
    super::from_hir::tidy(func, &order);
    super::cse::dce(func);
    hits
}

/// SCCP is on unless `COIL_MIR_SCCP=0` (or `false` / `off` / `no`).
fn enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| !matches!(std::env::var("COIL_MIR_SCCP").as_deref(), Ok("0" | "false" | "off" | "no")))
}

/// Values SCCP may replace by a `Const`: pure scalar computations. A call
/// or a load whose value happens to be known (it is not, in this lattice)
/// keeps its effect.
fn foldable(inst: &MirInst) -> bool {
    matches!(
        inst,
        MirInst::Bin { .. } | MirInst::Cmp { .. } | MirInst::Unary { .. } | MirInst::Cast { .. } | MirInst::Phi { .. }
    )
}

type Solution = (Vec<Lat>, HashSet<BlockId>, HashSet<(BlockId, BlockId)>);

/// The lattice value of every value, the blocks that run, and the edges
/// that run. `None` when it does not settle within [`MAX_SWEEPS`].
fn solve(func: &MirFunc) -> Option<Solution> {
    // Parameters and values bound by a terminator (`JumpIfMatch`
    // payloads) vary; values an instruction defines start unknown.
    let mut vals = vec![Lat::Bottom; func.types.len()];
    for block in &func.blocks {
        for inst in &block.insts {
            for d in inst.dests() {
                if let Some(v) = vals.get_mut(d.index()) {
                    *v = Lat::Top;
                }
            }
        }
    }
    let mut reached: HashSet<BlockId> = HashSet::from([func.entry]);
    let mut edges: HashSet<(BlockId, BlockId)> = HashSet::new();
    for _ in 0..MAX_SWEEPS {
        let mut changed = false;
        for block in &func.blocks {
            if !reached.contains(&block.id) {
                continue;
            }
            for inst in &block.insts {
                let new = eval(inst, block.id, &vals, &edges);
                for d in inst.dests() {
                    let Some(old) = vals.get(d.index()).copied() else { continue };
                    // Values only fall: ⊤ → constant → ⊥.
                    let next = if inst.dests().count() > 1 { Lat::Bottom } else { meet_down(old, new) };
                    if next != old {
                        vals[d.index()] = next;
                        changed = true;
                    }
                }
            }
            let succs: Vec<BlockId> = match &block.term {
                Some(Terminator::Br { cond, taken, not_taken }) => match vals.get(cond.index()).copied() {
                    Some(Lat::Top) => Vec::new(),
                    Some(Lat::Const(MirConst::Bool(c))) => vec![if c { *taken } else { *not_taken }],
                    _ => vec![*taken, *not_taken],
                },
                Some(t) => t.succs(),
                None => Vec::new(),
            };
            for s in succs {
                changed |= edges.insert((block.id, s));
                changed |= reached.insert(s);
            }
        }
        if !changed {
            return Some((vals, reached, edges));
        }
    }
    None
}

/// `old` lowered by `new`, never raised.
fn meet_down(old: Lat, new: Lat) -> Lat {
    match (old, new) {
        (_, Lat::Top) => old,
        (Lat::Top, x) => x,
        (Lat::Const(a), Lat::Const(b)) if a == b => old,
        _ => Lat::Bottom,
    }
}

/// What `inst` (in `block`) computes over the lattice.
fn eval(inst: &MirInst, block: BlockId, vals: &[Lat], edges: &HashSet<(BlockId, BlockId)>) -> Lat {
    let get = |v: ValueId| vals.get(v.index()).copied().unwrap_or(Lat::Bottom);
    match inst {
        MirInst::Const { c, .. } => Lat::Const(*c),
        MirInst::Phi { args, .. } => args
            .iter()
            .filter(|(from, _)| edges.contains(&(*from, block)))
            .fold(Lat::Top, |acc, &(_, v)| meet(acc, get(v))),
        i if foldable(i) => {
            let ops = i.operands();
            if ops.iter().any(|&v| get(v) == Lat::Top) {
                return Lat::Top;
            }
            let consts: HashMap<ValueId, MirConst> = ops
                .iter()
                .filter_map(|&v| match get(v) {
                    Lat::Const(c) => Some((v, c)),
                    _ => None,
                })
                .collect();
            // With an operand varying, `x * 0` and friends still fold.
            super::instcombine::const_of(i, &consts).map_or(Lat::Bottom, Lat::Const)
        }
        _ => Lat::Bottom,
    }
}

#[cfg(test)]
#[path = "sccp.tests.rs"]
mod tests;
