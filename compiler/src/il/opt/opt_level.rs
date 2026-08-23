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
    /// DCE, jump threading, copy/mem forwarding. Tiny-inline stays modest.
    Basic,
    /// All currently-on production passes. Backward-compatible default.
    #[default]
    Standard,
    /// Standard plus `seek_back_edge` and a larger inline budget.
    Aggressive,
    /// Standard with unrolling and return cloning off (less code growth).
    Size,
    /// Basic cleanup only; no slot promotion, escape SROA, unroll, or GVN.
    Debug,
}

impl OptLevel {
    /// Parse CLI / config tokens, including `-O2` / `O2` / `2` / `standard`.
    pub fn parse(name: &str) -> Result<Self, ()> {
        name.parse()
    }

    /// `OptimizeOptions` for this level.
    pub fn options(self) -> OptimizeOptions {
        match self {
            Self::None => none_opts(),
            Self::Basic => basic_opts(),
            Self::Standard => OptimizeOptions::default(),
            Self::Aggressive => {
                let mut o = OptimizeOptions::default();
                o.seek_back_edge = true;
                o
            }
            Self::Size => {
                let mut o = OptimizeOptions::default();
                o.loop_unroll = false;
                o.clone_shared_return = false;
                o
            }
            Self::Debug => debug_opts(),
        }
    }

    /// Tiny-inline budgets. Lives here so CLI tests can check mapping without
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

impl FromStr for OptLevel {
    type Err = ();

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
            _ => return Err(()),
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
        jump_thread: false,
        dead_block: false,
        stack_dce: false,
        mem_fwd: false,
        copy_prop: false,
        slot_promote: false,
        canon: false,
        cast_spill: false,
        algebraic: false,
        licm: false,
        loop_bounds: false,
        return_convoy: false,
        clone_shared_return: false,
        bin_join_convoy: false,
        multi_op_join_convoy: false,
        invert_guard_branch: false,
        slot_promote_tell: false,
        seek_back_edge: false,
        loop_unroll: false,
        loop_unroll_factor: 8,
        pgo_prioritize_hot_loops: false,
        invariant_store_elim: false,
        ssa_gvn: false,
        escape_analysis: false,
        branch_optimization: false,
        block_reordering: false,
        iterative_optimization: false,
        max_optimization_iterations: 10,
        collect_stats: false,
    }
}

fn none_opts() -> OptimizeOptions {
    let mut o = all_off();
    o.algebraic = true;
    o
}

fn basic_opts() -> OptimizeOptions {
    let mut o = none_opts();
    o.jump_thread = true;
    o.dead_block = true;
    o.stack_dce = true;
    o.mem_fwd = true;
    o.copy_prop = true;
    o
}

fn debug_opts() -> OptimizeOptions {
    basic_opts()
}

#[cfg(test)]
fn flag_vec(o: &OptimizeOptions) -> Vec<bool> {
    vec![
        o.jump_thread,
        o.dead_block,
        o.stack_dce,
        o.mem_fwd,
        o.copy_prop,
        o.slot_promote,
        o.canon,
        o.cast_spill,
        o.algebraic,
        o.licm,
        o.loop_bounds,
        o.return_convoy,
        o.clone_shared_return,
        o.bin_join_convoy,
        o.multi_op_join_convoy,
        o.invert_guard_branch,
        o.slot_promote_tell,
        o.seek_back_edge,
        o.loop_unroll,
        o.invariant_store_elim,
        o.ssa_gvn,
        o.escape_analysis,
        o.branch_optimization,
        o.block_reordering,
        o.iterative_optimization,
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
        assert!(!o.jump_thread);
        assert!(!o.dead_block);
        assert!(!o.slot_promote);
        assert!(!o.escape_analysis);
        assert!(!o.loop_unroll);
        assert!(!o.seek_back_edge);
    }

    #[test]
    fn basic_enables_dce_and_forwarding() {
        let o = OptLevel::Basic.options();
        assert!(o.algebraic && o.jump_thread && o.dead_block && o.stack_dce);
        assert!(o.mem_fwd && o.copy_prop);
        assert!(!o.licm && !o.slot_promote && !o.ssa_gvn && !o.escape_analysis);
    }

    #[test]
    fn aggressive_is_standard_plus_seek() {
        let s = OptLevel::Standard.options();
        let a = OptLevel::Aggressive.options();
        assert!(!s.seek_back_edge);
        assert!(a.seek_back_edge);
        let mut s2 = s.clone();
        s2.seek_back_edge = true;
        assert_eq!(a, s2);
    }

    #[test]
    fn size_disables_growth_passes() {
        let o = OptLevel::Size.options();
        assert!(!o.loop_unroll);
        assert!(!o.clone_shared_return);
        assert!(o.algebraic && o.dead_block && o.escape_analysis);
    }

    #[test]
    fn debug_preserves_slots() {
        let o = OptLevel::Debug.options();
        assert!(o.algebraic && o.dead_block);
        assert!(!o.slot_promote && !o.slot_promote_tell);
        assert!(!o.escape_analysis && !o.ssa_gvn && !o.loop_unroll);
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
