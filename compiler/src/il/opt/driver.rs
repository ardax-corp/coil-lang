//! Static opt pass driver (D2).
//!
//! Production passes live in [`PRODUCTION_PASSES`] (order matches D1 README).
//! The driver walks that table; a pass runs when its [`OptimizeOptions`] flag
//! is on (`dead_store` shares `mem_fwd`). [`PassDelta`] is what `collect_stats`
//! records — `PassKind` is data on the row, not a match in the loop.
//!
//! `IlModule::optimize_and_flatten` still defers `multi_op_join_convoy`,
//! `invert_guard_branch`, `seek_back_edge`, `slot_promote_tell`, and `ssa_gvn`
//! around per-body `cfg_gvn`. Those are not folded into this table. Fuse-select stays
//! in `lower_optimized`.

use super::super::op::IlOp;
use super::OptimizeOptions;
use super::stats::{self, PassDelta, PassKind};

/// Context threaded through one pipeline round. Keep this small.
pub struct PassCtx<'a> {
    pub entry_sp: i32,
    pub entry_tell: u32,
    pub pool: &'a mut Vec<u64>,
    pub next_label: &'a mut u32,
}

/// One named rewrite over a function body (or bare `Vec<IlOp>`).
pub trait Pass {
    fn name(&self) -> &'static str;
    fn run(&self, ops: &mut Vec<IlOp>, opts: &OptimizeOptions, ctx: &mut PassCtx<'_>) -> PassDelta;
}

/// Cleanup (profile-agnostic) vs decision (layout / heat).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Cleanup,
    Decision,
}

/// First level in `None ⊂ Basic ⊂ Standard ⊂ Aggressive` that enables a pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum OptFloor {
    None,
    Basic,
    Standard,
    Aggressive,
}

/// One row of [`PRODUCTION_PASSES`].
pub struct PassSpec {
    pub name: &'static str,
    pub phase: Phase,
    pub kind: PassKind,
    pub floor: OptFloor,
    /// Size omits growth passes (`loop_unroll`, `clone_shared_return`).
    pub omit_from_size: bool,
    /// After this row, seed `entry_tell` from `entry_sp` (the `mem_fwd` slot).
    pub seed_entry_tell_after: bool,
    gate: fn(&OptimizeOptions) -> bool,
    set_flag: fn(&mut OptimizeOptions),
    apply: fn(&mut Vec<IlOp>, &OptimizeOptions, &mut PassCtx<'_>) -> usize,
}

impl PassSpec {
    pub fn enabled(&self, opts: &OptimizeOptions) -> bool {
        (self.gate)(opts)
    }

    pub fn enable(&self, opts: &mut OptimizeOptions) {
        (self.set_flag)(opts);
    }
}

impl Pass for PassSpec {
    fn name(&self) -> &'static str {
        self.name
    }

    fn run(&self, ops: &mut Vec<IlOp>, opts: &OptimizeOptions, ctx: &mut PassCtx<'_>) -> PassDelta {
        stats::measure_pass(
            ops,
            opts.collect_stats,
            Pass::name(self),
            self.kind,
            |ops| (self.apply)(ops, opts, ctx),
        )
    }
}

/// Names of table rows whose flag is on, in table order.
#[cfg(test)]
pub fn enabled_pass_names(opts: &OptimizeOptions) -> Vec<&'static str> {
    PRODUCTION_PASSES
        .iter()
        .filter(|p| p.enabled(opts))
        .map(|p| p.name())
        .collect()
}

/// One pipeline round: cleanup, then decision.
pub fn run_once(
    ops: &mut Vec<IlOp>,
    opts: &OptimizeOptions,
    entry_sp: i32,
    pool: &mut Vec<u64>,
    next_label: &mut u32,
) {
    let mut ctx = PassCtx {
        entry_sp,
        // Same formula as today; copy_prop onward uses it. Re-seeded after
        // the mem_fwd row (even if that pass is off).
        entry_tell: entry_sp.max(0) as u32,
        pool,
        next_label,
    };
    run_phase(Phase::Cleanup, ops, opts, &mut ctx);
    run_phase(Phase::Decision, ops, opts, &mut ctx);
}

fn run_phase(phase: Phase, ops: &mut Vec<IlOp>, opts: &OptimizeOptions, ctx: &mut PassCtx<'_>) {
    for spec in PRODUCTION_PASSES {
        if spec.phase != phase {
            continue;
        }
        if spec.enabled(opts) {
            let delta = spec.run(ops, opts, ctx);
            if opts.collect_stats {
                stats::collect_delta(&delta);
            }
        }
        if spec.seed_entry_tell_after {
            ctx.entry_tell = ctx.entry_sp.max(0) as u32;
        }
    }
}

// Apply wrappers. Extra (unroll / branch / block-order counts) is the usize.

fn apply_jump_thread(ops: &mut Vec<IlOp>, _: &OptimizeOptions, _: &mut PassCtx<'_>) -> usize {
    super::cfg::jump_thread(ops);
    0
}

fn apply_dead_block(ops: &mut Vec<IlOp>, _: &OptimizeOptions, _: &mut PassCtx<'_>) -> usize {
    super::cfg::eliminate_dead_blocks(ops);
    0
}

fn apply_stack_dce(ops: &mut Vec<IlOp>, _: &OptimizeOptions, _: &mut PassCtx<'_>) -> usize {
    super::dce::stack_dce(ops);
    0
}

fn apply_mem_fwd(ops: &mut Vec<IlOp>, _: &OptimizeOptions, ctx: &mut PassCtx<'_>) -> usize {
    super::dce::mem_fwd(ops, ctx.entry_sp);
    0
}

fn apply_copy_prop(ops: &mut Vec<IlOp>, _: &OptimizeOptions, ctx: &mut PassCtx<'_>) -> usize {
    super::dce::copy_prop(ops, ctx.entry_tell);
    0
}

fn apply_dead_store(ops: &mut Vec<IlOp>, _: &OptimizeOptions, ctx: &mut PassCtx<'_>) -> usize {
    super::dce::dead_store_at(ops, ctx.entry_tell);
    0
}

fn apply_canon(ops: &mut Vec<IlOp>, _: &OptimizeOptions, ctx: &mut PassCtx<'_>) -> usize {
    crate::il::canon::canonicalize_operand_order(ops, ctx.pool);
    0
}

fn apply_algebraic(ops: &mut Vec<IlOp>, _: &OptimizeOptions, ctx: &mut PassCtx<'_>) -> usize {
    crate::il::algebraic::algebraic_simplify(ops, ctx.pool);
    0
}

fn apply_instcombine(ops: &mut Vec<IlOp>, _: &OptimizeOptions, _: &mut PassCtx<'_>) -> usize {
    super::instcombine::instcombine(ops)
}

fn apply_cast_spill(ops: &mut Vec<IlOp>, _: &OptimizeOptions, _: &mut PassCtx<'_>) -> usize {
    crate::il::cast_spill::spill_cast_before_float_chain(ops);
    0
}

fn apply_licm(ops: &mut Vec<IlOp>, opts: &OptimizeOptions, _: &mut PassCtx<'_>) -> usize {
    crate::il::licm::licm_with(ops, opts.pure_call_ctx.as_ref());
    0
}

fn apply_loop_bounds(ops: &mut Vec<IlOp>, opts: &OptimizeOptions, _: &mut PassCtx<'_>) -> usize {
    crate::il::bounds::loop_bounds_with(ops, opts.pure_call_ctx.as_ref());
    0
}

fn apply_strength_reduce(ops: &mut Vec<IlOp>, opts: &OptimizeOptions, ctx: &mut PassCtx<'_>) -> usize {
    crate::il::strength::strength_reduce(ops, ctx.pool, opts.pure_call_ctx.as_ref())
}

fn apply_loop_unroll(ops: &mut Vec<IlOp>, opts: &OptimizeOptions, _: &mut PassCtx<'_>) -> usize {
    super::loop_unroll::unroll_loops(ops, opts.loop_unroll_factor)
}

fn apply_invariant_store_elim(
    ops: &mut Vec<IlOp>,
    _: &OptimizeOptions,
    ctx: &mut PassCtx<'_>,
) -> usize {
    super::invariant_store_elim::eliminate_invariant_stores(ops, ctx.entry_sp);
    0
}

fn apply_ssa_gvn(ops: &mut Vec<IlOp>, _: &OptimizeOptions, _: &mut PassCtx<'_>) -> usize {
    crate::il::gvn_ssa::ssa_gvn(ops);
    0
}

fn apply_escape_analysis(
    ops: &mut Vec<IlOp>,
    _: &OptimizeOptions,
    _: &mut PassCtx<'_>,
) -> usize {
    super::escape_analysis::escape_analysis(ops);
    0
}

fn apply_slot_promote(ops: &mut Vec<IlOp>, _: &OptimizeOptions, ctx: &mut PassCtx<'_>) -> usize {
    super::slot_promote::slot_promote(ops, ctx.entry_tell);
    super::dce::dead_store_at(ops, ctx.entry_tell);
    0
}

fn apply_tos_carry(ops: &mut Vec<IlOp>, _: &OptimizeOptions, ctx: &mut PassCtx<'_>) -> usize {
    super::tos_carry::tos_carry(ops, ctx.entry_sp);
    0
}

fn apply_clone_shared_return(
    ops: &mut Vec<IlOp>,
    _: &OptimizeOptions,
    _: &mut PassCtx<'_>,
) -> usize {
    super::convoy::clone_shared_return(ops);
    0
}

fn apply_return_convoy(ops: &mut Vec<IlOp>, _: &OptimizeOptions, _: &mut PassCtx<'_>) -> usize {
    super::convoy::return_convoy(ops);
    0
}

fn apply_bin_join_convoy(ops: &mut Vec<IlOp>, _: &OptimizeOptions, _: &mut PassCtx<'_>) -> usize {
    super::convoy::bin_join_convoy(ops);
    0
}

fn apply_multi_op_join_convoy(
    ops: &mut Vec<IlOp>,
    _: &OptimizeOptions,
    _: &mut PassCtx<'_>,
) -> usize {
    super::convoy::multi_op_join_convoy(ops);
    0
}

fn apply_invert_guard_branch(
    ops: &mut Vec<IlOp>,
    _: &OptimizeOptions,
    _: &mut PassCtx<'_>,
) -> usize {
    super::cfg::invert_branch_over_jump(ops);
    0
}

fn apply_branch_optimization(
    ops: &mut Vec<IlOp>,
    _: &OptimizeOptions,
    ctx: &mut PassCtx<'_>,
) -> usize {
    super::branch_opt::optimize_branches_at(ops, ctx.entry_sp, ctx.next_label)
}

fn apply_block_reordering(ops: &mut Vec<IlOp>, _: &OptimizeOptions, _: &mut PassCtx<'_>) -> usize {
    super::block_order::reorder_basic_blocks(ops)
}

fn apply_seek_back_edge(ops: &mut Vec<IlOp>, _: &OptimizeOptions, ctx: &mut PassCtx<'_>) -> usize {
    super::slot_promote::seek_normalize_back_edges(ops, ctx.entry_tell);
    0
}

fn apply_slot_promote_tell(
    ops: &mut Vec<IlOp>,
    _: &OptimizeOptions,
    ctx: &mut PassCtx<'_>,
) -> usize {
    super::slot_promote::slot_promote_at(ops, ctx.entry_tell);
    0
}

/// Production opt passes. Order matches D1 README.
pub static PRODUCTION_PASSES: &[PassSpec] = &[
    PassSpec {
        name: "jump_thread",
        phase: Phase::Cleanup,
        kind: PassKind::Generic,
        floor: OptFloor::Basic,
        omit_from_size: false,
        seed_entry_tell_after: false,
        gate: |o| o.jump_thread,
        set_flag: |o| o.jump_thread = true,
        apply: apply_jump_thread,
    },
    PassSpec {
        name: "dead_block",
        phase: Phase::Cleanup,
        kind: PassKind::Generic,
        floor: OptFloor::Basic,
        omit_from_size: false,
        seed_entry_tell_after: false,
        gate: |o| o.dead_block,
        set_flag: |o| o.dead_block = true,
        apply: apply_dead_block,
    },
    PassSpec {
        name: "stack_dce",
        phase: Phase::Cleanup,
        kind: PassKind::Generic,
        floor: OptFloor::Basic,
        omit_from_size: false,
        seed_entry_tell_after: false,
        gate: |o| o.stack_dce,
        set_flag: |o| o.stack_dce = true,
        apply: apply_stack_dce,
    },
    PassSpec {
        name: "mem_fwd",
        phase: Phase::Cleanup,
        kind: PassKind::Generic,
        floor: OptFloor::Basic,
        omit_from_size: false,
        seed_entry_tell_after: true,
        gate: |o| o.mem_fwd,
        set_flag: |o| o.mem_fwd = true,
        apply: apply_mem_fwd,
    },
    PassSpec {
        name: "copy_prop",
        phase: Phase::Cleanup,
        kind: PassKind::Generic,
        floor: OptFloor::Basic,
        omit_from_size: false,
        seed_entry_tell_after: false,
        gate: |o| o.copy_prop,
        set_flag: |o| o.copy_prop = true,
        apply: apply_copy_prop,
    },
    PassSpec {
        name: "dead_store",
        phase: Phase::Cleanup,
        kind: PassKind::Generic,
        floor: OptFloor::Basic,
        omit_from_size: false,
        seed_entry_tell_after: false,
        gate: |o| o.mem_fwd,
        set_flag: |o| o.mem_fwd = true,
        apply: apply_dead_store,
    },
    PassSpec {
        name: "canon",
        phase: Phase::Cleanup,
        kind: PassKind::Generic,
        floor: OptFloor::Standard,
        omit_from_size: false,
        seed_entry_tell_after: false,
        gate: |o| o.canon,
        set_flag: |o| o.canon = true,
        apply: apply_canon,
    },
    PassSpec {
        name: "algebraic",
        phase: Phase::Cleanup,
        kind: PassKind::Generic,
        floor: OptFloor::None,
        omit_from_size: false,
        seed_entry_tell_after: false,
        gate: |o| o.algebraic,
        set_flag: |o| o.algebraic = true,
        apply: apply_algebraic,
    },
    PassSpec {
        name: "instcombine",
        phase: Phase::Cleanup,
        kind: PassKind::Generic,
        floor: OptFloor::Standard,
        omit_from_size: false,
        seed_entry_tell_after: false,
        gate: |o| o.instcombine,
        set_flag: |o| o.instcombine = true,
        apply: apply_instcombine,
    },
    PassSpec {
        name: "cast_spill",
        phase: Phase::Cleanup,
        kind: PassKind::Generic,
        floor: OptFloor::Standard,
        omit_from_size: false,
        seed_entry_tell_after: false,
        gate: |o| o.cast_spill,
        set_flag: |o| o.cast_spill = true,
        apply: apply_cast_spill,
    },
    PassSpec {
        name: "licm",
        phase: Phase::Decision,
        kind: PassKind::Generic,
        floor: OptFloor::Standard,
        omit_from_size: false,
        seed_entry_tell_after: false,
        gate: |o| o.licm,
        set_flag: |o| o.licm = true,
        apply: apply_licm,
    },
    PassSpec {
        name: "loop_bounds",
        phase: Phase::Decision,
        kind: PassKind::Generic,
        floor: OptFloor::Standard,
        omit_from_size: false,
        seed_entry_tell_after: false,
        gate: |o| o.loop_bounds,
        set_flag: |o| o.loop_bounds = true,
        apply: apply_loop_bounds,
    },
    PassSpec {
        name: "strength_reduce",
        phase: Phase::Decision,
        kind: PassKind::Generic,
        floor: OptFloor::Standard,
        omit_from_size: false,
        seed_entry_tell_after: false,
        gate: |o| o.strength_reduce,
        set_flag: |o| o.strength_reduce = true,
        apply: apply_strength_reduce,
    },
    PassSpec {
        name: "loop_unroll",
        phase: Phase::Decision,
        kind: PassKind::Unroll,
        floor: OptFloor::Standard,
        omit_from_size: true,
        seed_entry_tell_after: false,
        gate: |o| o.loop_unroll,
        set_flag: |o| o.loop_unroll = true,
        apply: apply_loop_unroll,
    },
    PassSpec {
        name: "invariant_store_elim",
        phase: Phase::Decision,
        kind: PassKind::Generic,
        floor: OptFloor::Standard,
        omit_from_size: false,
        seed_entry_tell_after: false,
        gate: |o| o.invariant_store_elim,
        set_flag: |o| o.invariant_store_elim = true,
        apply: apply_invariant_store_elim,
    },
    PassSpec {
        name: "escape_analysis",
        phase: Phase::Decision,
        kind: PassKind::Generic,
        floor: OptFloor::Standard,
        omit_from_size: false,
        seed_entry_tell_after: false,
        gate: |o| o.escape_analysis,
        set_flag: |o| o.escape_analysis = true,
        apply: apply_escape_analysis,
    },
    PassSpec {
        name: "slot_promote",
        phase: Phase::Decision,
        kind: PassKind::Generic,
        floor: OptFloor::Standard,
        omit_from_size: false,
        seed_entry_tell_after: false,
        gate: |o| o.slot_promote,
        set_flag: |o| o.slot_promote = true,
        apply: apply_slot_promote,
    },
    PassSpec {
        name: "tos_carry",
        phase: Phase::Decision,
        kind: PassKind::Generic,
        floor: OptFloor::Standard,
        omit_from_size: false,
        seed_entry_tell_after: false,
        gate: |o| o.tos_carry,
        set_flag: |o| o.tos_carry = true,
        apply: apply_tos_carry,
    },
    PassSpec {
        name: "clone_shared_return",
        phase: Phase::Decision,
        kind: PassKind::Generic,
        floor: OptFloor::Standard,
        omit_from_size: true,
        seed_entry_tell_after: false,
        gate: |o| o.clone_shared_return,
        set_flag: |o| o.clone_shared_return = true,
        apply: apply_clone_shared_return,
    },
    PassSpec {
        name: "return_convoy",
        phase: Phase::Decision,
        kind: PassKind::Generic,
        floor: OptFloor::Standard,
        omit_from_size: false,
        seed_entry_tell_after: false,
        gate: |o| o.return_convoy,
        set_flag: |o| o.return_convoy = true,
        apply: apply_return_convoy,
    },
    PassSpec {
        name: "bin_join_convoy",
        phase: Phase::Decision,
        kind: PassKind::Generic,
        floor: OptFloor::Standard,
        omit_from_size: false,
        seed_entry_tell_after: false,
        gate: |o| o.bin_join_convoy,
        set_flag: |o| o.bin_join_convoy = true,
        apply: apply_bin_join_convoy,
    },
    PassSpec {
        name: "multi_op_join_convoy",
        phase: Phase::Decision,
        kind: PassKind::Generic,
        floor: OptFloor::Standard,
        omit_from_size: false,
        seed_entry_tell_after: false,
        gate: |o| o.multi_op_join_convoy,
        set_flag: |o| o.multi_op_join_convoy = true,
        apply: apply_multi_op_join_convoy,
    },
    PassSpec {
        name: "invert_guard_branch",
        phase: Phase::Decision,
        kind: PassKind::Generic,
        floor: OptFloor::Standard,
        omit_from_size: false,
        seed_entry_tell_after: false,
        gate: |o| o.invert_guard_branch,
        set_flag: |o| o.invert_guard_branch = true,
        apply: apply_invert_guard_branch,
    },
    PassSpec {
        name: "branch_optimization",
        phase: Phase::Decision,
        kind: PassKind::Branch,
        floor: OptFloor::Standard,
        omit_from_size: false,
        seed_entry_tell_after: false,
        gate: |o| o.branch_optimization,
        set_flag: |o| o.branch_optimization = true,
        apply: apply_branch_optimization,
    },
    PassSpec {
        name: "block_reordering",
        phase: Phase::Decision,
        kind: PassKind::BlockOrder,
        floor: OptFloor::Standard,
        omit_from_size: false,
        seed_entry_tell_after: false,
        gate: |o| o.block_reordering,
        set_flag: |o| o.block_reordering = true,
        apply: apply_block_reordering,
    },
    PassSpec {
        name: "seek_back_edge",
        phase: Phase::Decision,
        kind: PassKind::Generic,
        floor: OptFloor::Aggressive,
        omit_from_size: false,
        seed_entry_tell_after: false,
        gate: |o| o.seek_back_edge,
        set_flag: |o| o.seek_back_edge = true,
        apply: apply_seek_back_edge,
    },
    PassSpec {
        name: "slot_promote_tell",
        phase: Phase::Decision,
        kind: PassKind::Generic,
        floor: OptFloor::Standard,
        omit_from_size: false,
        seed_entry_tell_after: false,
        gate: |o| o.slot_promote_tell,
        set_flag: |o| o.slot_promote_tell = true,
        apply: apply_slot_promote_tell,
    },
    PassSpec {
        name: "ssa_gvn",
        phase: Phase::Decision,
        kind: PassKind::Generic,
        floor: OptFloor::Standard,
        omit_from_size: false,
        seed_entry_tell_after: false,
        gate: |o| o.ssa_gvn,
        set_flag: |o| o.ssa_gvn = true,
        apply: apply_ssa_gvn,
    },
];

/// D1 README production order (cleanup then decision).
#[cfg(test)]
pub const D1_PASS_ORDER: &[&str] = &[
    "jump_thread",
    "dead_block",
    "stack_dce",
    "mem_fwd",
    "copy_prop",
    "dead_store",
    "canon",
    "algebraic",
    "instcombine",
    "cast_spill",
    "licm",
    "loop_bounds",
    "strength_reduce",
    "loop_unroll",
    "invariant_store_elim",
    "escape_analysis",
    "slot_promote",
    "tos_carry",
    "clone_shared_return",
    "return_convoy",
    "bin_join_convoy",
    "multi_op_join_convoy",
    "invert_guard_branch",
    "branch_optimization",
    "block_reordering",
    "seek_back_edge",
    "slot_promote_tell",
    "ssa_gvn",
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::il::opt::OptLevel;

    #[test]
    fn production_table_matches_d1_order() {
        let names: Vec<_> = PRODUCTION_PASSES.iter().map(|p| p.name).collect();
        assert_eq!(names, D1_PASS_ORDER);
    }

    #[test]
    fn driver_walks_enabled_standard_passes_in_d1_order() {
        let opts = OptimizeOptions::default();
        let enabled = enabled_pass_names(&opts);
        assert_eq!(enabled, OptLevel::Standard.pass_names());
        assert!(
            !enabled.contains(&"seek_back_edge"),
            "Standard leaves seek_back_edge off"
        );
        assert_eq!(enabled, subsequence(D1_PASS_ORDER, &enabled));
        assert_eq!(
            enabled,
            [
                "jump_thread",
                "dead_block",
                "stack_dce",
                "mem_fwd",
                "copy_prop",
                "dead_store",
                "canon",
                "algebraic",
                "instcombine",
                "licm",
                "loop_bounds",
                "strength_reduce",
                "loop_unroll",
                "invariant_store_elim",
                "escape_analysis",
                "slot_promote",
                "tos_carry",
                "clone_shared_return",
                "return_convoy",
                "bin_join_convoy",
                "multi_op_join_convoy",
                "invert_guard_branch",
                "branch_optimization",
                "block_reordering",
                "slot_promote_tell",
                "ssa_gvn",
            ]
        );
    }

    #[test]
    fn iterative_opt_reuses_the_same_table() {
        // optimize_iteratively_at re-runs run_once, which walks PRODUCTION_PASSES.
        assert_eq!(
            PRODUCTION_PASSES.len(),
            D1_PASS_ORDER.len(),
            "iterative rounds walk the same production table"
        );
        let names: Vec<_> = PRODUCTION_PASSES.iter().map(|p| p.name).collect();
        assert_eq!(names, D1_PASS_ORDER);
    }

    fn subsequence<'a>(order: &[&'a str], enabled: &[&'a str]) -> Vec<&'a str> {
        order
            .iter()
            .copied()
            .filter(|n| enabled.contains(n))
            .collect()
    }
}
