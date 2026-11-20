//! Per-body MIR specialization.

use crate::opt::OptimizeOptions;

/// The MIR passes run on a body at `opts`' level: the folds, loop passes,
/// SROA and the loop rewrites (`copy_until`, `byte_scan`, `bulk_push`,
/// `vec_loops`). Both engines take this MIR (`native_capture`).
pub fn optimize_mir(func: &mut crate::mir::MirFunc, opts: &OptimizeOptions) {
    // `-O0` / `-Og` run none of these; `-O1` only the folds.
    if opts.mir_fold {
        crate::mir::sccp(func);
        check_after(func, "sccp");
    }
    if opts.mir_loop {
        loop_opts(func, true);
        // A nest walking memory across its rows walks along them instead;
        // the inner loop's row base is then invariant: hoist it.
        if crate::mir::interchange_loops(func) > 0 {
            check_after(func, "interchange_loops");
            loop_opts(func, true);
        }
    }
    if opts.mir_fold {
        crate::mir::instcombine(func);
        check_after(func, "instcombine");
        crate::mir::destprop(func);
        check_after(func, "destprop");
    }
    if opts.mir_loop {
        crate::mir::strength_reduce(func);
        check_after(func, "strength_reduce");
        crate::mir::cse(func);
        check_after(func, "cse");
        crate::mir::gvn(func);
        check_after(func, "gvn");
    }
    // S2f: reuse the mutated array after StoreIndex (drop rematerialized Alloc).
    crate::mir::sroa(func);
    check_after(func, "sroa");
    // An object made on each path into a φ and only read field by field
    // after it (`find_in(..) ?? new Span(..)`) is its fields' φs.
    crate::mir::phi_sroa(func);
    check_after(func, "phi_sroa");
    // `a && b` joins on a `bool` φ: each way in branches for itself.
    if opts.mir_fold && crate::mir::thread_bool_phis(func) > 0 {
        check_after(func, "thread_bool_phis");
    }
    // A loop that pushes each byte until one stops it: a scan for the
    // stop (which `byte_scan` takes next) and one copy of the run.
    if opts.mir_loop && crate::mir::copy_until(func) > 0 {
        check_after(func, "copy_until");
    }
    // Early-exit byte loops skip 64 no-op iterations at a time. Their
    // range setup is invariant in any outer loop: hoist it.
    if opts.mir_loop && crate::mir::byte_scan(func) > 0 {
        loop_opts(func, true);
        check_after(func, "byte_scan");
    }
    // Counted copy and fill loops run as one call.
    if opts.mir_loop && crate::mir::bulk_push(func) > 0 {
        check_after(func, "bulk_push");
    }
    // Counted map and reduce loops run 8 lanes at a time in one op.
    if opts.mir_loop && crate::mir::vec_loops(func) > 0 {
        check_after(func, "vec_loops");
    }
    // Hints only: forward scans of pointer arrays prefetch ahead.
    if opts.mir_loop {
        crate::mir::mark_prefetch_scans(func);
        check_after(func, "mark_prefetch_scans");
    }
}

/// The scalar passes both tiers run first: local CSE, trivial-φ copy
/// forwarding (a loop-carried copy of an invariant reads as invariant),
/// loop-invariant code motion, then CSE again over what moved together.
/// `dense`: the body emits dense ([`crate::mir::licm`]).
fn loop_opts(func: &mut crate::mir::MirFunc, dense: bool) {
    let cse = if dense { crate::mir::cse } else { crate::mir::cse_lir };
    cse(func);
    check_after(func, "cse");
    crate::mir::destprop(func);
    check_after(func, "destprop");
    crate::mir::licm(func, dense);
    check_after(func, "licm");
    cse(func);
    check_after(func, "cse");
}

/// The variable that turns the MIR verifier on in any build.
pub const MIR_VERIFY_ENV: &str = "COIL_MIR_VERIFY";

/// `COIL_MIR_VERIFY` is set (to anything but nothing or `0`): every body is
/// verified as lowered and after each pass, in release builds too. A pass
/// that breaks SSA there shows as a hang or a wrong value at best (#2422).
pub(crate) fn verify_asked() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| asks(std::env::var_os(MIR_VERIFY_ENV).as_deref()))
}

/// Whether a `COIL_MIR_VERIFY` value turns the verifier on.
fn asks(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|v| !v.is_empty() && v != "0")
}

/// Debug builds, and any build under `COIL_MIR_VERIFY`, verify the MIR
/// after each pass, so a pass that breaks SSA is named where it does
/// instead of surfacing as a bad emit (#1869).
fn check_after(func: &crate::mir::MirFunc, pass: &str) {
    if cfg!(debug_assertions) || verify_asked() {
        verify_or_panic(func, pass);
    }
}

/// Under `COIL_MIR_VERIFY`, verify a body as an engine gets it from
/// lowering, before any pass: what `-O0` and the interpreter run.
pub(super) fn check_lowered(func: &crate::mir::MirFunc) {
    if verify_asked() {
        verify_or_panic(func, "lowering");
    }
}

/// Under `COIL_MIR_VERIFY`, check that every use in an expanded body
/// ([`crate::mir::native_expand`]) is dominated by its definition. Not
/// [`crate::mir::MirFunc::verify`]: that one also wants a definition ahead
/// of its uses in block order, and an expansion's join block, which defines
/// the call's result, is laid out after the block that goes on to use it.
pub(super) fn check_expanded(func: &crate::mir::MirFunc) {
    if verify_asked()
        && let Err(e) = func.verify_dominance()
    {
        panic!("MIR verify failed after native_expand in {}: {e}", func.name);
    }
}

fn verify_or_panic(func: &crate::mir::MirFunc, pass: &str) {
    if let Err(e) = func.verify().and_then(|()| func.verify_dominance()) {
        panic!("MIR verify failed after {pass} in {}: {e}", func.name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn the_variable_is_off_when_unset_empty_or_zero() {
        assert!(!asks(None));
        assert!(!asks(Some(OsStr::new(""))));
        assert!(!asks(Some(OsStr::new("0"))));
        assert!(asks(Some(OsStr::new("1"))));
        assert!(asks(Some(OsStr::new("on"))));
    }

    /// The header's value defined a second time: what LICM did in #2422.
    #[test]
    #[should_panic(expected = "MIR verify failed after licm in twice: value v2 defined twice")]
    fn a_violation_panics_with_the_pass_and_the_function() {
        let src = "func @twice(v0: f64, v1: f64) -> f64 {\nbb0:\n    v2 = fmul v0, v1\n    return v2\n}\n";
        let mut func = crate::mir::parse_func(src).expect(src);
        let again = func.blocks[0].insts[0].clone();
        func.blocks[0].insts.push(again);
        verify_or_panic(&func, "licm");
    }
}
