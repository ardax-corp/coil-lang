//! Named optimization presets (`-O0` … `-O3`, `-Os`, `-Og`).
//!
//! `None ⊂ Basic ⊂ Standard ⊂ Aggressive` on enable flags. `Size` and `Debug`
//! are independent axes (code size vs. preserving named slots / debug shape).

use std::fmt;
use std::str::FromStr;

use super::OptimizeOptions;

/// Compiler optimization level. Default is [`Self::Standard`] (current pipeline).
///
/// Wired through [`crate::Pipeline::set_opt_level`]. Canonical names serialize
/// as lowercase strings (`"standard"`) for config files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OptLevel {
    /// Algebraic / const-fold peeps only.
    None,
    /// Constant folding only, with modest inlining.
    Basic,
    /// Every production pass. The default.
    #[default]
    Standard,
    /// Standard plus a larger inline budget.
    Aggressive,
    /// Standard with unrolling and return sinking off (less code growth).
    Size,
    /// Constant folding only; no scalar replacement, unroll or inlining.
    Debug,
}

impl OptLevel {
    /// Parse CLI / config tokens, including `-O2` / `O2` / `2` / `standard`.
    pub fn parse(name: &str) -> Result<Self, BadOptLevel> {
        name.parse()
    }

    /// `OptimizeOptions` for this level.
    pub fn options(self) -> OptimizeOptions {
        base_knobs(self)
    }

    /// Typed-inlining budgets. Lives here so CLI tests can check mapping without
    /// constructing a `Compiler` (codegen would create an IL cycle).
    pub fn inline_max_cost(self) -> usize {
        match self {
            Self::None | Self::Debug => 0,
            Self::Basic => 25,
            Self::Standard => 100,
            Self::Aggressive => 200,
            Self::Size => 40,
        }
    }

    pub fn inline_across_modules(self) -> bool {
        matches!(self, Self::Standard | Self::Aggressive | Self::Size)
    }
}

/// `OptLevel::parse` rejected the token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BadOptLevel;

impl FromStr for OptLevel {
    type Err = BadOptLevel;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let t = s.trim();
        let t = t
            .strip_prefix("-O")
            .or_else(|| t.strip_prefix("-o"))
            .or_else(|| t.strip_prefix('O'))
            .unwrap_or(t)
            .trim();
        Ok(match t.to_ascii_lowercase().as_str() {
            "none" | "0" | "n" => Self::None,
            "basic" | "1" => Self::Basic,
            "standard" | "2" => Self::Standard,
            "aggressive" | "3" => Self::Aggressive,
            "size" | "s" => Self::Size,
            "debug" | "g" => Self::Debug,
            _ => return Err(BadOptLevel),
        })
    }
}

impl fmt::Display for OptLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::None => "none",
            Self::Basic => "basic",
            Self::Standard => "standard",
            Self::Aggressive => "aggressive",
            Self::Size => "size",
            Self::Debug => "debug",
        })
    }
}

fn all_off() -> OptimizeOptions {
    OptimizeOptions {
        algebraic: false,
        loop_bounds: false,
        sink_return: false,
        loop_unroll: false,
        loop_unroll_factor: 8,
        escape_analysis: false,
        branch_optimization: false,
        collect_stats: false,
        call_fold: false,
        mir_fold: false,
        mir_loop: false,
        hir_inline: false,
        hir_inline_heap: false,
        hir_pair_locals: false,
        hir_bisect: None,
        auto_par: false,
        require_mir: false,
    }
}

/// The switches no level sets: on at every level (a setter may turn them
/// off), and `COIL_HIR_BISECT`, read once per options built and only by
/// debug builds (release `coil` never bisects).
fn level_free_knobs(o: &mut OptimizeOptions) {
    o.hir_inline = true;
    o.hir_inline_heap = true;
    o.hir_pair_locals = true;
    o.hir_bisect = if cfg!(debug_assertions) {
        std::env::var("COIL_HIR_BISECT").ok().and_then(|v| v.parse::<usize>().ok())
    } else {
        None
    };
    o.auto_par = true;
}

impl OptimizeOptions {
    /// Take `from`'s level-independent switches (the [`level_free_knobs`]
    /// ones, which a setter may have changed since): a new level keeps them.
    pub(crate) fn keep_knobs(&mut self, from: &OptimizeOptions) {
        self.hir_inline = from.hir_inline;
        self.hir_inline_heap = from.hir_inline_heap;
        self.hir_pair_locals = from.hir_pair_locals;
        self.hir_bisect = from.hir_bisect;
        self.auto_par = from.auto_par;
    }
}

/// The switches `level` turns on.
fn base_knobs(level: OptLevel) -> OptimizeOptions {
    let mut o = all_off();
    level_free_knobs(&mut o);
    // Gate HIR passes in `emit_hir`, not IL table rows: constant folding (at
    // every level), scalar replacement (enum / tuple SROA), counted-loop
    // bounds proofs and full unroll of short counted loops (not at Size: it
    // grows code).
    o.algebraic = true;
    let standard = matches!(
        level,
        OptLevel::Standard | OptLevel::Aggressive | OptLevel::Size
    );
    o.escape_analysis = standard;
    o.loop_bounds = standard;
    o.loop_unroll = matches!(level, OptLevel::Standard | OptLevel::Aggressive);
    // Return in each branch of a returned `match` (not at Size: a two-word
    // return repeats per branch).
    o.sink_return = o.loop_unroll;
    // Lay out early exits after the body (`emit_hir`).
    o.branch_optimization = standard;
    o.call_fold = standard;
    // MIR: no pass but what emission needs at None and Debug, folding
    // only at Basic.
    o.mir_fold = !matches!(level, OptLevel::None | OptLevel::Debug);
    o.mir_loop = standard;
    o
}

impl Default for OptimizeOptions {
    fn default() -> Self {
        OptLevel::Standard.options()
    }
}

#[cfg(test)]
fn flag_vec(o: &OptimizeOptions) -> Vec<bool> {
    vec![
        o.algebraic,
        o.loop_bounds,
        o.sink_return,
        o.loop_unroll,
        o.escape_analysis,
        o.branch_optimization,
        o.mir_fold,
        o.mir_loop,
    ]
}

#[cfg(test)]
fn is_flag_subset(lo: &OptimizeOptions, hi: &OptimizeOptions) -> bool {
    flag_vec(lo)
        .into_iter()
        .zip(flag_vec(hi))
        .all(|(a, b)| !a || b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_names_and_shorthands() {
        assert_eq!(OptLevel::parse("none").unwrap(), OptLevel::None);
        assert_eq!(OptLevel::parse("0").unwrap(), OptLevel::None);
        assert_eq!(OptLevel::parse("basic").unwrap(), OptLevel::Basic);
        assert_eq!(OptLevel::parse("1").unwrap(), OptLevel::Basic);
        assert_eq!(OptLevel::parse("standard").unwrap(), OptLevel::Standard);
        assert_eq!(OptLevel::parse("2").unwrap(), OptLevel::Standard);
        assert_eq!(OptLevel::parse("aggressive").unwrap(), OptLevel::Aggressive);
        assert_eq!(OptLevel::parse("3").unwrap(), OptLevel::Aggressive);
        assert_eq!(OptLevel::parse("size").unwrap(), OptLevel::Size);
        assert_eq!(OptLevel::parse("s").unwrap(), OptLevel::Size);
        assert_eq!(OptLevel::parse("debug").unwrap(), OptLevel::Debug);
        assert_eq!(OptLevel::parse("g").unwrap(), OptLevel::Debug);
        assert!(OptLevel::parse("fast").is_err());
        assert_eq!(OptLevel::default(), OptLevel::Standard);
        assert_eq!(OptLevel::parse("-O2").unwrap(), OptLevel::Standard);
        assert_eq!(OptLevel::parse("-O0").unwrap(), OptLevel::None);
        assert_eq!(OptLevel::parse("-Os").unwrap(), OptLevel::Size);
        assert_eq!(OptLevel::parse("-Og").unwrap(), OptLevel::Debug);
        assert_eq!(OptLevel::parse("O3").unwrap(), OptLevel::Aggressive);
        assert_eq!(OptLevel::Standard.to_string(), "standard");
        let json = serde_json::to_string(&OptLevel::Standard).unwrap();
        assert_eq!(json, "\"standard\"");
        let back: OptLevel = serde_json::from_str(&json).unwrap();
        assert_eq!(back, OptLevel::Standard);
    }

    #[test]
    fn standard_matches_optimize_options_default() {
        assert_eq!(OptLevel::Standard.options(), OptimizeOptions::default());
    }

    #[test]
    fn none_is_algebraic_only() {
        let o = OptLevel::None.options();
        assert!(o.algebraic);
        assert!(!o.escape_analysis);
        assert!(!o.loop_unroll);
    }

    #[test]
    fn basic_folds_only() {
        let o = OptLevel::Basic.options();
        assert!(o.algebraic);
        assert!(!o.loop_bounds && !o.escape_analysis);
    }

    #[test]
    fn aggressive_runs_the_standard_passes() {
        assert_eq!(OptLevel::Standard.options(), OptLevel::Aggressive.options());
    }

    #[test]
    fn size_disables_growth_passes() {
        let o = OptLevel::Size.options();
        assert!(!o.loop_unroll);
        assert!(!o.sink_return);
        assert!(o.algebraic && o.escape_analysis);
    }

    #[test]
    fn debug_preserves_slots() {
        let o = OptLevel::Debug.options();
        assert!(o.algebraic);
        assert!(!o.escape_analysis && !o.loop_unroll);
    }

    #[test]
    fn mir_passes_follow_the_level() {
        for level in [OptLevel::None, OptLevel::Debug] {
            let o = level.options();
            assert!(!o.mir_fold && !o.mir_loop, "{level:?} runs no MIR pass");
        }
        let o = OptLevel::Basic.options();
        assert!(o.mir_fold && !o.mir_loop);
        for level in [OptLevel::Standard, OptLevel::Aggressive, OptLevel::Size] {
            let o = level.options();
            assert!(o.mir_fold && o.mir_loop, "{level:?} runs every MIR pass");
        }
    }

    #[test]
    fn level_free_knobs_survive_a_level_change() {
        let mut set = OptLevel::Standard.options();
        set.hir_inline = false;
        set.auto_par = false;
        set.hir_bisect = Some(3);
        let mut o = OptLevel::None.options();
        o.keep_knobs(&set);
        assert!(!o.hir_inline && !o.auto_par);
        assert_eq!(o.hir_bisect, Some(3));
        assert!(!o.mir_fold && !o.mir_loop, "the level's own switches stay");
    }

    #[test]
    fn higher_levels_are_supersets() {
        let chain = [
            OptLevel::None,
            OptLevel::Basic,
            OptLevel::Standard,
            OptLevel::Aggressive,
        ];
        for w in chain.windows(2) {
            let lo = w[0].options();
            let hi = w[1].options();
            assert!(
                is_flag_subset(&lo, &hi),
                "{:?} flags must be a subset of {:?}",
                w[0],
                w[1]
            );
        }
    }

    #[test]
    fn inline_budgets() {
        assert_eq!(OptLevel::None.inline_max_cost(), 0);
        assert_eq!(OptLevel::Debug.inline_max_cost(), 0);
        assert_eq!(OptLevel::Basic.inline_max_cost(), 25);
        assert_eq!(OptLevel::Standard.inline_max_cost(), 100);
        assert_eq!(OptLevel::Aggressive.inline_max_cost(), 200);
        assert_eq!(OptLevel::Size.inline_max_cost(), 40);
        assert!(!OptLevel::None.inline_across_modules());
        assert!(!OptLevel::Basic.inline_across_modules());
        assert!(OptLevel::Standard.inline_across_modules());
        assert!(OptLevel::Aggressive.inline_across_modules());
    }
}
