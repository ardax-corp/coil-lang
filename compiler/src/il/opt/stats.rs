//! Optimization statistics (COI-131). Collection is off unless
//! [`super::OptimizeOptions::collect_stats`] is set.

use std::cell::RefCell;
use std::fmt::{self, Write as _};

use super::super::op::IlOp;

thread_local! {
    static LAST_STATS: RefCell<Option<OptStats>> = const { RefCell::new(None) };
}

/// How a named pass contributes to the ticket-level counters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PassKind {
    Generic,
    Unroll,
    Branch,
    BlockOrder,
}

/// One named pass that mutated the buffer (aggregated by name).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PassHit {
    pub name: String,
    pub applied: usize,
    pub ops_delta: i64,
}

/// Counters from IL opts (and tiny-inline when compiling). COI-176 `OptimizationStats`.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OptStats {
    pub ops_eliminated: usize,
    pub ops_added: usize,
    pub functions_inlined: usize,
    pub loops_unrolled: usize,
    pub loads_eliminated: usize,
    pub stores_eliminated: usize,
    pub branches_optimized: usize,
    pub blocks_reordered: usize,
    pub iterations: usize,
    pub passes: Vec<PassHit>,
}

impl OptStats {
    fn add_pass(&mut self, name: &'static str, ops_delta: i64) {
        self.merge_pass(name, 1, ops_delta);
    }

    fn merge_pass(&mut self, name: &str, applied: usize, ops_delta: i64) {
        if let Some(hit) = self.passes.iter_mut().find(|p| p.name == name) {
            hit.applied += applied;
            hit.ops_delta += ops_delta;
            return;
        }
        self.passes.push(PassHit {
            name: name.to_string(),
            applied,
            ops_delta,
        });
    }

    /// Fold `other` into `self` (COI-176).
    pub fn accumulate(&mut self, other: &Self) {
        self.ops_eliminated += other.ops_eliminated;
        self.ops_added += other.ops_added;
        self.functions_inlined += other.functions_inlined;
        self.loops_unrolled += other.loops_unrolled;
        self.loads_eliminated += other.loads_eliminated;
        self.stores_eliminated += other.stores_eliminated;
        self.branches_optimized += other.branches_optimized;
        self.blocks_reordered += other.blocks_reordered;
        self.iterations += other.iterations;
        for hit in &other.passes {
            self.merge_pass(&hit.name, hit.applied, hit.ops_delta);
        }
    }

    /// Alias for [`Self::accumulate`].
    pub fn add(&mut self, other: &Self) {
        self.accumulate(other);
    }

    /// Human-readable report for `--opt-stats`.
    pub fn format_text(&self) -> String {
        let mut out = String::from("optimization stats:\n");
        let _ = writeln!(out, "  iterations: {}", self.iterations);
        let _ = writeln!(out, "  ops eliminated: {}", self.ops_eliminated);
        let _ = writeln!(out, "  ops added: {}", self.ops_added);
        let _ = writeln!(out, "  functions inlined: {}", self.functions_inlined);
        let _ = writeln!(out, "  loops unrolled: {}", self.loops_unrolled);
        let _ = writeln!(out, "  loads eliminated: {}", self.loads_eliminated);
        let _ = writeln!(out, "  stores eliminated: {}", self.stores_eliminated);
        let _ = writeln!(out, "  branches optimized: {}", self.branches_optimized);
        let _ = writeln!(out, "  blocks reordered: {}", self.blocks_reordered);
        if self.passes.is_empty() {
            let _ = writeln!(out, "  passes: (none applied)");
        } else {
            let _ = writeln!(out, "  passes:");
            let mut ranked = self.passes.clone();
            ranked.sort_by(|a, b| b.applied.cmp(&a.applied).then(a.name.cmp(&b.name)));
            for hit in ranked {
                let _ = writeln!(
                    out,
                    "    {}: {} change(s), Δops {}",
                    hit.name, hit.applied, hit.ops_delta
                );
            }
        }
        out
    }

    /// Machine-readable report for `--opt-stats-json`.
    pub fn format_json(&self) -> String {
        let mut passes = String::new();
        for (i, hit) in self.passes.iter().enumerate() {
            if i > 0 {
                passes.push(',');
            }
            let _ = write!(
                passes,
                "{{\"name\":\"{}\",\"applied\":{},\"ops_delta\":{}}}",
                hit.name, hit.applied, hit.ops_delta
            );
        }
        format!(
            "{{\"ops_eliminated\":{},\"ops_added\":{},\"functions_inlined\":{},\"loops_unrolled\":{},\"loads_eliminated\":{},\"stores_eliminated\":{},\"branches_optimized\":{},\"blocks_reordered\":{},\"iterations\":{},\"passes\":[{}]}}",
            self.ops_eliminated,
            self.ops_added,
            self.functions_inlined,
            self.loops_unrolled,
            self.loads_eliminated,
            self.stores_eliminated,
            self.branches_optimized,
            self.blocks_reordered,
            self.iterations,
            passes
        )
    }
}

impl fmt::Display for OptStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.format_text())
    }
}

/// Start a fresh collection window (one compile).
pub fn begin_opt_stats() {
    LAST_STATS.with(|c| *c.borrow_mut() = Some(OptStats::default()));
}

/// Snapshot of the current window, or zeros if collection is off.
pub fn last_opt_stats() -> OptStats {
    LAST_STATS.with(|c| c.borrow().clone().unwrap_or_default())
}

pub(crate) fn note_function_inlined() {
    with_stats(|s| s.functions_inlined += 1);
}

pub(crate) fn set_iterations(n: usize) {
    with_stats(|s| s.iterations = n);
}

fn with_stats(f: impl FnOnce(&mut OptStats)) {
    LAST_STATS.with(|c| {
        if c.borrow().is_none() {
            *c.borrow_mut() = Some(OptStats::default());
        }
        if let Some(s) = c.borrow_mut().as_mut() {
            f(s);
        }
    });
}

fn count_loads(ops: &[IlOp]) -> usize {
    ops.iter()
        .filter(|op| matches!(op, IlOp::Load { .. } | IlOp::LoadReturnSlot { .. }))
        .count()
}

fn count_stores(ops: &[IlOp]) -> usize {
    ops.iter()
        .filter(|op| matches!(op, IlOp::StorePop { .. }))
        .count()
}

/// Run `f` and, when `collect`, record length / load / store deltas.
pub(crate) fn run_named_pass(
    ops: &mut Vec<IlOp>,
    collect: bool,
    name: &'static str,
    kind: PassKind,
    f: impl FnOnce(&mut Vec<IlOp>) -> usize,
) {
    if !collect {
        let _ = f(ops);
        return;
    }
    let before = ops.clone();
    let extra = f(ops);
    if *ops == before {
        return;
    }
    let delta = ops.len() as i64 - before.len() as i64;
    let load_delta = count_loads(ops) as i64 - count_loads(&before) as i64;
    let store_delta = count_stores(ops) as i64 - count_stores(&before) as i64;
    with_stats(|s| {
        if delta < 0 {
            s.ops_eliminated += (-delta) as usize;
        } else {
            s.ops_added += delta as usize;
        }
        if load_delta < 0 {
            s.loads_eliminated += (-load_delta) as usize;
        }
        if store_delta < 0 {
            s.stores_eliminated += (-store_delta) as usize;
        }
        match kind {
            PassKind::Generic => {}
            PassKind::Unroll => s.loops_unrolled += extra.max(1),
            PassKind::Branch => s.branches_optimized += extra.max(1),
            PassKind::BlockOrder => s.blocks_reordered += extra.max(1),
        }
        s.add_pass(name, delta);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulate_sums_counters_and_merges_passes() {
        let mut a = OptStats {
            ops_eliminated: 2,
            functions_inlined: 1,
            iterations: 1,
            passes: vec![PassHit {
                name: "dce".into(),
                applied: 1,
                ops_delta: -2,
            }],
            ..OptStats::default()
        };
        let b = OptStats {
            ops_eliminated: 3,
            loops_unrolled: 1,
            iterations: 2,
            passes: vec![
                PassHit {
                    name: "dce".into(),
                    applied: 1,
                    ops_delta: -1,
                },
                PassHit {
                    name: "unroll".into(),
                    applied: 1,
                    ops_delta: 8,
                },
            ],
            ..OptStats::default()
        };
        a.add(&b);
        assert_eq!(a.ops_eliminated, 5);
        assert_eq!(a.functions_inlined, 1);
        assert_eq!(a.loops_unrolled, 1);
        assert_eq!(a.iterations, 3);
        assert_eq!(a.passes.len(), 2);
        assert_eq!(a.passes[0].applied, 2);
        assert_eq!(a.passes[0].ops_delta, -3);
        let json = serde_json::to_string(&a).unwrap();
        assert!(json.contains("\"ops_eliminated\":5"));
        let round: OptStats = serde_json::from_str(&json).unwrap();
        assert_eq!(round, a);
    }
}
