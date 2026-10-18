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
