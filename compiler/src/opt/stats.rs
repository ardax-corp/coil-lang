//! Optimization statistics (COI-131): HIR lowering, typed inlining and
//! compile-time work, printed by `--opt-stats` / `--opt-stats-json`. A
//! window opens per compile ([`begin_opt_stats`]).

use std::cell::RefCell;
use std::fmt::{self, Write as _};

thread_local! {
    static LAST_STATS: RefCell<Option<OptStats>> = const { RefCell::new(None) };
}

/// How many times one reason was given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReasonCount {
    pub name: String,
    pub count: usize,
}

/// Counters from HIR lowering, typed inlining and compile-time code.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OptStats {
    /// Function bodies HIR lowering planned.
    pub hir_lowered: usize,
    /// Function bodies HIR lowering refused (a compile error unless an
    /// earlier error caused it).
    pub hir_refused: usize,
    /// Why each refused body was refused (coarse keys, counted).
    pub hir_refused_reasons: Vec<ReasonCount>,
    /// Bodies lowered straight from HIR to MIR.
    pub hir_mir: usize,
    /// Why the other planned bodies got no MIR (counted).
    pub hir_mir_refused: Vec<ReasonCount>,
    /// Call sites typed inlining spliced into HIR bodies.
    pub hir_inlined: usize,
    /// Bodies whose inlined HIR did not plan, by reason (kept un-inlined).
    pub hir_inline_refused: Vec<ReasonCount>,
    /// Compile-time code (comptime step 12): macro calls and `comptime`
    /// blocks run (compiling their programs included), and how many of them
    /// reused an earlier result; wall time in microseconds.
    pub comptime_macro_runs: usize,
    pub comptime_macro_cached: usize,
    pub comptime_macro_us: u64,
    /// `const`s and `comptime` expressions computed, how many came from the
    /// result cache, pure calls tried for folding, and the wall time.
    pub comptime_consts: usize,
    pub comptime_consts_cached: usize,
    pub comptime_folds: usize,
    pub comptime_const_us: u64,
}

/// `reasons`, most frequent first.
fn ranked(reasons: &[ReasonCount]) -> Vec<ReasonCount> {
    let mut ranked = reasons.to_vec();
    ranked.sort_by(|a, b| b.count.cmp(&a.count).then(a.name.cmp(&b.name)));
    ranked
}

impl OptStats {
    /// Human-readable report for `--opt-stats`.
    pub fn format_text(&self) -> String {
        let mut out = String::from("optimization stats:\n");
        if self.hir_lowered + self.hir_refused > 0 {
            let _ = writeln!(out, "  hir: {} lowered, {} refused", self.hir_lowered, self.hir_refused);
            for hit in ranked(&self.hir_refused_reasons) {
                let _ = writeln!(out, "    {}: {}", hit.name, hit.count);
            }
        }
        if self.hir_mir + self.hir_mir_refused.len() > 0 {
            let _ = writeln!(out, "  hir mir: {} lowered", self.hir_mir);
            for hit in ranked(&self.hir_mir_refused) {
                let _ = writeln!(out, "    {}: {}", hit.name, hit.count);
            }
        }
        if self.hir_inlined + self.hir_inline_refused.len() > 0 {
            let _ = writeln!(out, "  hir inline: {} sites", self.hir_inlined);
            for hit in ranked(&self.hir_inline_refused) {
                let _ = writeln!(out, "    refused {}: {}", hit.name, hit.count);
            }
        }
        if self.comptime_macro_runs + self.comptime_consts + self.comptime_folds > 0 {
            let ms = |us: u64| us as f64 / 1000.0;
            let _ = writeln!(
                out,
                "  comptime: {:.1} ms\n    macros and blocks: {} run, {} reused, {:.1} ms\n    consts: {} computed, {} from the cache, {} pure calls tried, {:.1} ms",
                ms(self.comptime_macro_us + self.comptime_const_us),
                self.comptime_macro_runs,
                self.comptime_macro_cached,
                ms(self.comptime_macro_us),
                self.comptime_consts,
                self.comptime_consts_cached,
                self.comptime_folds,
                ms(self.comptime_const_us)
            );
        }
        out
    }

    /// Machine-readable report for `--opt-stats-json`.
    pub fn format_json(&self) -> String {
        format!(
            "{{\"hir_lowered\":{},\"hir_refused\":{},\"hir_mir\":{},\"hir_inlined\":{},\"comptime_macro_runs\":{},\"comptime_macro_cached\":{},\"comptime_macro_us\":{},\"comptime_consts\":{},\"comptime_consts_cached\":{},\"comptime_folds\":{},\"comptime_const_us\":{}}}",
            self.hir_lowered,
            self.hir_refused,
            self.hir_mir,
            self.hir_inlined,
            self.comptime_macro_runs,
            self.comptime_macro_cached,
            self.comptime_macro_us,
            self.comptime_consts,
            self.comptime_consts_cached,
            self.comptime_folds,
            self.comptime_const_us,
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

fn note_reason(reasons: &mut Vec<ReasonCount>, name: &str) {
    if let Some(hit) = reasons.iter_mut().find(|h| h.name == name) {
        hit.count += 1;
    } else {
        reasons.push(ReasonCount { name: name.to_string(), count: 1 });
    }
}

/// Count one function body lowered from HIR.
pub(crate) fn note_hir_lowered() {
    with_stats(|s| s.hir_lowered += 1);
}

/// Count one function body HIR lowering refused, and why.
pub(crate) fn note_hir_refused(reason: &str) {
    with_stats(|s| {
        s.hir_refused += 1;
        note_reason(&mut s.hir_refused_reasons, reason);
    });
}

/// Count one body lowered straight to MIR, or why it was not.
pub(crate) fn note_hir_mir(refused: Option<&str>) {
    with_stats(|s| match refused {
        None => s.hir_mir += 1,
        Some(why) => note_reason(&mut s.hir_mir_refused, why),
    });
}

/// Count `sites` call sites inlined into one HIR body.
pub(crate) fn note_hir_inlined(sites: usize) {
    with_stats(|s| s.hir_inlined += sites);
}

/// Count one body whose inlined HIR was refused, and why.
pub(crate) fn note_hir_inline_refused(reason: &str) {
    with_stats(|s| note_reason(&mut s.hir_inline_refused, reason));
}

/// Count one round of macro and `comptime` block runs (`reused` of
/// `runs` from a cache) that took `us` microseconds.
pub(crate) fn note_comptime_macros(runs: usize, reused: usize, us: u64) {
    with_stats(|s| {
        s.comptime_macro_runs += runs;
        s.comptime_macro_cached += reused;
        s.comptime_macro_us += us;
    });
}

/// Count one const evaluation: `consts` computed (`cached` from the result
/// cache) and `folds` pure calls tried, in `us` microseconds.
pub(crate) fn note_comptime_consts(consts: usize, cached: usize, folds: usize, us: u64) {
    with_stats(|s| {
        s.comptime_consts += consts;
        s.comptime_consts_cached += cached;
        s.comptime_folds += folds;
        s.comptime_const_us += us;
    });
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

