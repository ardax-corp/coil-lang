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
        local_cse: false,
        licm: false,
        loop_bounds: false,
        sink_return: false,
        loop_unroll: false,
        loop_unroll_factor: 8,
        escape_analysis: false,
        branch_optimization: false,
        collect_stats: false,
        mir_specialize: false,
    }
}

/// The switches `level` turns on.
fn base_knobs(level: OptLevel) -> OptimizeOptions {
    let mut o = all_off();
    o.mir_specialize = true;
    // Gate HIR passes in `emit_hir`, not IL table rows: constant folding (at
    // every level), scalar replacement (enum / tuple SROA), local CSE,
    // loop-invariant code motion, counted-loop bounds proofs and full unroll
    // of short counted loops (not at Size: it grows code).
    o.algebraic = true;
    let standard = matches!(
        level,
        OptLevel::Standard | OptLevel::Aggressive | OptLevel::Size
    );
    o.escape_analysis = standard;
    o.local_cse = standard;
    o.licm = standard;
    o.loop_bounds = standard;
    o.loop_unroll = matches!(level, OptLevel::Standard | OptLevel::Aggressive);
    // Return in each branch of a returned `match` (not at Size: a two-word
    // return repeats per branch).
    o.sink_return = o.loop_unroll;
    // Lay out early exits after the body (`emit_hir`).
    o.branch_optimization = standard;
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
        o.local_cse,
        o.licm,
        o.loop_bounds,
        o.sink_return,
        o.loop_unroll,
        o.escape_analysis,
        o.branch_optimization,
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
        assert!(!o.local_cse);
    }

    #[test]
    fn basic_folds_only() {
        let o = OptLevel::Basic.options();
        assert!(o.algebraic);
        assert!(!o.licm && !o.escape_analysis);
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
        assert!(o.mir_specialize);
        assert!(OptLevel::Standard.options().mir_specialize);
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
