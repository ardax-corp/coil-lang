//! Tier-gate facts read off a body's MIR (N1b).
//!
//! The dense / LIR gates ask whether a body allocates, where its loops are
//! and which reconstruct walls it holds. Every body takes its stack IL
//! from MIR, so the gates read them here. The questions match their
//! stack-IL forms ([`super::gc::refuse_reason`], `loop_ranges`)
//! instruction for instruction.

use std::collections::HashSet;

use super::entry::LirRefuse;
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

/// The MIR form of [`super::gc::refuse_reason`]: `Make*` / `InitTyped` /
/// `MakeDict` / `DictEntries` / `MakeCoro`, `ArrayPush`, `STRINGIFY`, and
/// `FORMAT` with arguments. `BoxValue` / `MakeFn` / `MakePolyFn` are not.
pub fn refuses_alloc(inst: &MirInst) -> bool {
    match inst {
        MirInst::Alloc { kind, .. } => {
            !matches!(kind, MirAllocKind::Boxed { .. } | MirAllocKind::Fn { .. } | MirAllocKind::PolyFn { .. } | MirAllocKind::PolyFnCapture { .. })
        }
        MirInst::ArrayPush { .. } | MirInst::Stringify { .. } => true,
        MirInst::Format { args, .. } => !args.is_empty(),
        _ => false,
    }
}

/// `func` allocates where a stack map site binds: a [`refuses_alloc`]
/// site, or an argless `FORMAT` ([`MirFunc::has_mapped_gc_edge`]).
pub fn maps_allocation(func: &MirFunc) -> bool {
    func.blocks.iter().flat_map(|b| b.insts.iter()).any(refuses_alloc) || func.has_mapped_gc_edge()
}

/// Call, host or format work (the stack IL's `CALL | HOST | FORMAT` effects).
fn does_work(inst: &MirInst) -> bool {
    matches!(
        inst,
        MirInst::Call { .. }
            | MirInst::CallIndirect { .. }
            | MirInst::HostInvoke { .. }
            | MirInst::Ffi { .. }
            | MirInst::Print { .. }
            | MirInst::Format { .. }
            | MirInst::Stringify { .. }
            | MirInst::ResumeCoro { .. }
            | MirInst::Alloc {
                kind: MirAllocKind::Coro { .. } | MirAllocKind::Fn { .. } | MirAllocKind::PolyFn { .. } | MirAllocKind::PolyFnCapture { .. },
                ..
            }
    )
}

impl GateFacts {
    pub fn of(func: &MirFunc) -> Self {
        let has_alloc = func.blocks.iter().flat_map(|b| b.insts.iter()).any(refuses_alloc);
        let loops = natural_loops(func);
        if loops.is_empty() {
            return Self { has_alloc, ..Self::default() };
        }
        let in_loop: HashSet<BlockId> = loops.iter().flat_map(|lp| lp.blocks.iter().copied()).collect();
        let allocates = |b: BlockId| func.block(b).insts.iter().any(refuses_alloc);
        let alloc_in_loop = in_loop.iter().any(|&b| allocates(b));
        let post_loop_alloc_return = has_alloc && !alloc_in_loop && {
            // Blocks no loop is reachable from: the tail after the loops.
            let preds = func.preds();
            let mut before: HashSet<BlockId> = in_loop.clone();
            let mut work: Vec<BlockId> = in_loop.iter().copied().collect();
            while let Some(b) = work.pop() {
                for &p in &preds[b.index()] {
                    if before.insert(p) {
                        work.push(p);
                    }
                }
            }
            let tail = func.blocks.iter().filter(|b| !before.contains(&b.id));
            func.blocks.iter().filter(|b| before.contains(&b.id)).all(|b| !allocates(b.id))
                && !tail.flat_map(|b| b.insts.iter()).any(does_work)
        };
        Self {
            has_alloc,
            has_loop: true,
            alloc_in_loop,
            post_loop_alloc_return,
        }
    }
}

/// The LIR walls of a body: neither a user call nor a `BoxValue` is one
/// (the S2b map leaves the box to the precise frame map), only an
/// allocation without maps (`maps_ok`).
pub fn fuse_lir_wall(func: &MirFunc, maps_ok: bool) -> Option<LirRefuse> {
    let insts = || func.blocks.iter().flat_map(|b| b.insts.iter());
    if !maps_ok && insts().any(refuses_alloc) {
        return Some(LirRefuse::Alloc);
    }
    None
}

/// Highest label `func` names (call / closure / coroutine targets): fresh
/// labels a reconstruct mints start above it.
pub fn max_named_label(func: &MirFunc) -> u32 {
    super::emit::foreign_targets(func, None).into_iter().max().unwrap_or(0)
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

    #[test]
    fn boxed_value_is_no_alloc_and_no_wall() {
        let text = "func @f(v0: heapref) -> heapref {
bb0:
    v1 = alloc.boxed 3, v0
    v2 = gcbarrier.safepoint v1
    return v2
}";
        let f = parse_func(text).expect(text);
        assert_eq!(GateFacts::of(&f), GateFacts::default());
        assert_eq!(fuse_lir_wall(&f, false), None);
    }
}
