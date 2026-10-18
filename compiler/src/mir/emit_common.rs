//! Helpers shared by the dense ([`super::emit`]) and LIR
//! ([`super::emit_lir`]) emitters. One copy here, so a fix lands in both
//! tiers.

use super::func::MirFunc;
use super::inst::{BlockId, MirInst, Terminator, ValueId};

/// `dest` is a payload a `JumpIfMatch` leaves on the stack when taken.
pub(super) fn is_jim_term_payload(func: &MirFunc, dest: ValueId) -> bool {
    func.blocks.iter().any(|b| {
        matches!(
            &b.term,
            Some(Terminator::JumpIfMatch { payloads, .. }) if payloads.contains(&dest)
        )
    })
}

/// The `MatchPayload` dests of `scrutinee` in `block`, by payload index,
/// that no `JumpIfMatch` leaves on the stack: the last arm's `Unpack`.
pub(super) fn last_arm_payloads(func: &MirFunc, block: BlockId, scrutinee: ValueId) -> Vec<ValueId> {
    let mut group = Vec::new();
    for inst in &func.block(block).insts {
        let MirInst::MatchPayload {
            dest,
            scrutinee: src,
            index,
        } = inst
        else {
            continue;
        };
        if *src != scrutinee || is_jim_term_payload(func, *dest) {
            continue;
        }
        let i = *index as usize;
        if group.len() <= i {
            group.resize(i + 1, *dest);
        }
        group[i] = *dest;
    }
    group
}

/// The register moves the phis of `succ` need on the edge from `pred`,
/// sequentialized with `scratch`.
pub(super) fn phi_moves(
    func: &MirFunc,
    pred: BlockId,
    succ: BlockId,
    regs: &[u8],
    scratch: u8,
) -> Vec<(u8, u8)> {
    let mut moves = Vec::new();
    for inst in &func.block(succ).insts {
        let MirInst::Phi { dest, args, .. } = inst else {
            break;
        };
        if let Some((_, src)) = args.iter().find(|(b, _)| *b == pred) {
            let d = regs[dest.index()];
            let s = regs[src.index()];
            if d != s {
                moves.push((d, s));
            }
        }
    }
    resolve_parallel(moves, scratch)
}

/// Sequentialize parallel copies with one scratch (cycle break).
fn resolve_parallel(mut moves: Vec<(u8, u8)>, scratch: u8) -> Vec<(u8, u8)> {
    let mut out = Vec::new();
    while !moves.is_empty() {
        if let Some(i) = moves.iter().position(|(d, _)| !moves.iter().any(|(_, s)| s == d)) {
            out.push(moves.remove(i));
            continue;
        }
        let (d0, s0) = moves.remove(0);
        out.push((scratch, d0));
        out.push((d0, s0));
        for (_, s) in moves.iter_mut() {
            if *s == d0 {
                *s = scratch;
            }
        }
    }
    out
}
