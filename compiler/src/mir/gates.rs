//! Tier-gate facts read off a body's MIR (N1b).
//!
//! The dense / LIR gates ask whether a body allocates, where its loops are
//! and which reconstruct walls it holds. Every body takes its stack IL
//! from MIR, so the gates read them here. The questions match their
//! stack-IL forms ([`super::gc::refuse_reason`], `loop_ranges`)
//! instruction for instruction.

use std::collections::HashSet;
use super::func::MirFunc;
use super::inst::{BlockId, MirAllocKind, MirInst};
use super::licm::natural_loops;

/// What the tier gates ask of a body.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GateFacts {
    /// An allocation or GC safepoint ([`super::gc::refuse_reason`]).
    pub has_alloc: bool,
    /// The body has a loop.
    pub has_loop: bool,
    /// An allocation inside a loop.
    pub alloc_in_loop: bool,
    /// Every allocation sits after the loops, in a tail that only shapes
    /// the result (`return [sum]`: no call / host / format work).
    pub post_loop_alloc_return: bool,
}

impl GateFacts {
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mir::parse_func;

    /// A counted loop over `v0`, with `pre` before it and `post` after it.
    fn looped(pre: &str, post: &str, ret: &str) -> MirFunc {
        let text = format!(
            "func @f(v0: i64) -> heapref {{
bb0:
    v1 = iconst.i64 0
{pre}    jump bb1
bb1:
    v2 = phi.i64 [bb0: v1, bb2: v4]
    v3 = icmp.slt v2, v0
    brif v3, bb2, bb3
bb2:
    v5 = iconst.i64 1
    v4 = iadd v2, v5
    jump bb1
bb3:
{post}    return {ret}
}}"
        );
        parse_func(&text).expect(&text)
    }

    /// The GC map draft gate reads MIR: an argless `FORMAT` binds a map
    /// site though it is no `refuses_alloc` site; a function value alone
    /// binds none.
    #[test]
    fn maps_allocation_counts_argless_format_not_fn_values() {
        let fmt = parse_func("func @f(v0: heapref) -> heapref {\nbb0:\n    v1 = format v0\n    return v1\n}").unwrap();
        assert!(!GateFacts::of(&fmt).has_alloc);
        assert!(maps_allocation(&fmt));
        let fnv = parse_func("func @f(v0: i64) -> heapref {\nbb0:\n    v1 = alloc.fn 7, 3, v0\n    v2 = gcbarrier.safepoint v1\n    return v2\n}").unwrap();
        assert!(!maps_allocation(&fnv));
    }

    #[test]
    fn alloc_after_the_loop_shapes_the_result() {
        let f = looped("", "    v10 = alloc.array v2\n    v11 = gcbarrier.safepoint v10\n", "v11");
        let facts = GateFacts::of(&f);
        assert!(facts.has_alloc && facts.has_loop && !facts.alloc_in_loop);
        assert!(facts.post_loop_alloc_return);
    }

    #[test]
    fn preheader_alloc_is_not_post_loop() {
        let f = looped("    v10 = alloc.array v0\n    v11 = gcbarrier.safepoint v10\n", "", "v11");
        let facts = GateFacts::of(&f);
        assert!(facts.has_alloc && !facts.alloc_in_loop);
        assert!(!facts.post_loop_alloc_return);
    }

    #[test]
    fn tail_call_work_keeps_the_body() {
        let post = "    v12 = call.i64 7 v2\n    v10 = alloc.array v12\n    v11 = gcbarrier.safepoint v10\n";
        let facts = GateFacts::of(&looped("", post, "v11"));
        assert!(facts.has_alloc && !facts.post_loop_alloc_return);
    }
}
