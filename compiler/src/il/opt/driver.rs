//! Static opt pass driver (D2).
//!
//! Production passes live in [`PRODUCTION_PASSES`] (order matches D1 README).
//! The driver walks that table; a pass runs when its [`OptimizeOptions`] flag
//! is on. [`PassDelta`] is what `collect_stats`
//! records — `PassKind` is data on the row, not a match in the loop.
//!
//! `IlModule::optimize_and_flatten` runs this table on each function body.
//! Fuse-select stays in `lower_optimized`.

use super::super::op::IlOp;
use super::OptimizeOptions;
use super::stats::{self, PassDelta, PassKind};

/// Context threaded through one pipeline round. Keep this small.
pub struct PassCtx {
    pub entry_tell: u32,
}

/// One named rewrite over a function body (or bare `Vec<IlOp>`).
pub trait Pass {
    fn name(&self) -> &'static str;
    fn run(&self, ops: &mut Vec<IlOp>, opts: &OptimizeOptions, ctx: &mut PassCtx) -> PassDelta;
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
    /// Size omits growth passes (`clone_shared_return`).
    pub omit_from_size: bool,
    gate: fn(&OptimizeOptions) -> bool,
    set_flag: fn(&mut OptimizeOptions),
    apply: ApplyFn,
}

/// A pass body over a function's ops (growth passes splice / push).
enum ApplyFn {
    Grow(fn(&mut Vec<IlOp>, &OptimizeOptions, &mut PassCtx) -> usize),
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

    fn run(&self, ops: &mut Vec<IlOp>, opts: &OptimizeOptions, ctx: &mut PassCtx) -> PassDelta {
        stats::measure_pass(
            ops,
            opts.collect_stats,
            Pass::name(self),
            self.kind,
            |ops| match self.apply {
                ApplyFn::Grow(apply) => apply(ops, opts, ctx),
            },
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
pub fn run_once(ops: &mut Vec<IlOp>, opts: &OptimizeOptions, entry_sp: i32) {
    let mut ctx = PassCtx {
        // Cursor seed for the slot-tracking passes (`slot_promote`).
        entry_tell: entry_sp.max(0) as u32,
    };
    run_phase(Phase::Cleanup, ops, opts, &mut ctx);
    run_phase(Phase::Decision, ops, opts, &mut ctx);
}

fn run_phase(phase: Phase, ops: &mut Vec<IlOp>, opts: &OptimizeOptions, ctx: &mut PassCtx) {
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
    }
}

// Apply wrappers. The usize is a pass-specific count.

fn apply_dead_block(ops: &mut Vec<IlOp>, _: &OptimizeOptions, _: &mut PassCtx) -> usize {
    super::cfg::eliminate_dead_blocks(ops);
    0
}

fn apply_slot_promote(ops: &mut Vec<IlOp>, _: &OptimizeOptions, ctx: &mut PassCtx) -> usize {
    super::slot_promote::slot_promote(ops, ctx.entry_tell);
    super::dce::dead_store_at(ops, ctx.entry_tell);
    0
}

fn apply_clone_shared_return(
    ops: &mut Vec<IlOp>,
    _: &OptimizeOptions,
    _: &mut PassCtx,
) -> usize {
    super::convoy::clone_shared_return(ops);
    0
}

/// Production opt passes. Order matches D1 README.
pub static PRODUCTION_PASSES: &[PassSpec] = &[
    PassSpec {
        name: "dead_block",
        phase: Phase::Cleanup,
        kind: PassKind::Generic,
        floor: OptFloor::Basic,
        omit_from_size: false,
        gate: |o| o.dead_block,
        set_flag: |o| o.dead_block = true,
        apply: ApplyFn::Grow(apply_dead_block),
    },
    PassSpec {
        name: "slot_promote",
        phase: Phase::Decision,
        kind: PassKind::Generic,
        floor: OptFloor::Standard,
        omit_from_size: false,
        gate: |o| o.slot_promote,
        set_flag: |o| o.slot_promote = true,
        apply: ApplyFn::Grow(apply_slot_promote),
    },
    PassSpec {
        name: "clone_shared_return",
        phase: Phase::Decision,
        kind: PassKind::Generic,
        floor: OptFloor::Standard,
        omit_from_size: true,
        gate: |o| o.clone_shared_return,
        set_flag: |o| o.clone_shared_return = true,
        apply: ApplyFn::Grow(apply_clone_shared_return),
    },
];

/// D1 README production order (cleanup then decision).
#[cfg(test)]
pub const D1_PASS_ORDER: &[&str] = &[
    "dead_block",
    "slot_promote",
    "clone_shared_return",
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
        assert_eq!(enabled, subsequence(D1_PASS_ORDER, &enabled));
        assert_eq!(
            enabled,
            [
                "dead_block",
                            "slot_promote",
                "clone_shared_return",
            ]
        );
    }

    #[test]
    fn production_table_matches_documented_order() {
        assert_eq!(PRODUCTION_PASSES.len(), D1_PASS_ORDER.len());
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
