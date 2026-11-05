//! Emit a body's MIR (lowered from HIR) as dense bytecode when that wins,
//! else as LIR stack ops.


use crate::il::opt::OptimizeOptions;

use std::collections::HashMap;


type RefusalSlot = std::thread::LocalKey<std::cell::RefCell<Option<String>>>;

/// `(function, "dense" | "lir", MIR text)` captured for `coil dissect --mir`.
pub type CapturedMir = Vec<(String, &'static str, String)>;

thread_local! {
    /// `coil dissect --mir`: `(function, form, MIR text)` of each body that
    /// reached emission. `None` when not capturing.
    static MIR_CAPTURE: std::cell::RefCell<Option<CapturedMir>> =
        const { std::cell::RefCell::new(None) };
    static DENSE_REFUSAL: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
    static LIR_REFUSAL: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Start recording the final MIR of each specialized body (dissect).
pub fn start_mir_capture() {
    MIR_CAPTURE.with(|c| *c.borrow_mut() = Some(Vec::new()));
}

/// Stop recording and return `(function, "dense" | "lir", MIR text)`.
pub fn take_mir_capture() -> CapturedMir {
    MIR_CAPTURE.with(|c| c.borrow_mut().take().unwrap_or_default())
}

/// A pinned (`defer`) body's MIR from HIR, which no tier emits yet.
pub fn capture_pinned_mir(name: &str, func: &crate::mir::MirFunc) {
    capture_mir(name, "pinned", func);
}

fn capture_mir(name: &str, form: &'static str, func: &crate::mir::MirFunc) {
    MIR_CAPTURE.with(|c| {
        if let Some(list) = c.borrow_mut().as_mut() {
            list.push((name.to_string(), form, func.to_string()));
        }
    });
}

/// The MIR passes the dense tier runs on a body, at `opts`' level: the
/// folds, loop passes, SROA and the loop rewrites (`copy_until`,
/// `byte_scan`, `bulk_push`, `vec_loops`). Native code takes the same
/// MIR (`native_capture`). `promote`: keep loop-written fields in
/// registers (`promote_fields`, whose `PanicStore`s a backend must honour).
pub fn optimize_mir(func: &mut crate::mir::MirFunc, opts: &OptimizeOptions, promote: bool) {
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
    // A field a loop writes stays in a register across it; last, so no
    // pass moves code across the stores a panic owes the heap.
    if promote && opts.mir_loop && crate::mir::promote_fields(func) > 0 {
        check_after(func, "promote_fields");
    }
    // Hints only: forward scans of pointer arrays prefetch ahead.
    if opts.mir_loop {
        crate::mir::mark_prefetch_scans(func);
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

/// Debug builds verify the MIR after each pass, so a pass that breaks SSA
/// is named where it does instead of surfacing as a bad emit (#1869).
#[cfg(debug_assertions)]
fn check_after(func: &crate::mir::MirFunc, pass: &str) {
    if let Err(e) = func.verify().and_then(|()| func.verify_dominance()) {
        panic!("MIR verify failed after {pass} in {}: {e}", func.name);
    }
}

#[cfg(not(debug_assertions))]
fn check_after(_: &crate::mir::MirFunc, _: &str) {}

#[cfg(test)]
mod cost_gate_tests {
    use common::{Byte, DebugLoc, Instruction};

    use crate::il::IlOp;

    use super::residual_heap_box;

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

    #[test]
    fn residual_make_counts_as_boxed() {
        let ops = [IlOp::MakeArray { elem_kind: 0, arity: 2, loc: loc() }];
        assert!(residual_heap_box(&ops));
        let native = [IlOp::byte(
            Byte::new(Instruction::DenseMake).with_dense_abc(0, 1, 2, 3),
        )];
        assert!(!residual_heap_box(&native));
    }

    #[test]
    fn body_labels_read_only_the_labels() {
        use crate::il::Label;
        let ops = [
            IlOp::Label(Label(4)),
            IlOp::Label(Label(9)),
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Label(Label(12)),
        ];
        let labels = super::BodyLabels::of(&ops);
        assert_eq!(labels.first, Some(Label(4)));
        assert_eq!(labels.head, vec![Label(4), Label(9)]);
        assert_eq!(super::BodyLabels::of(&ops[2..]).first, Some(Label(12)));
        assert!(super::BodyLabels::of(&ops[2..]).head.is_empty());
    }

    /// A pass that leaves broken MIR is named in debug builds (#1869).
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "MIR verify failed after sroa in broken")]
    fn check_after_names_the_pass() {
        super::check_after(&crate::mir::MirFunc::new("broken"), "sroa");
    }

    /// A use one arm defines but the join reads passes the structural
    /// check; the dominance check catches it (#1869).
    #[test]
    fn verify_dominance_rejects_a_use_one_arm_defines() {
        let src = r#"
func @arm(v0: i64) -> i64 {
bb0:
    v1 = iconst.i64 0
    v2 = icmp.slt v0, v1
    brif v2, bb1, bb2
bb1:
    v3 = iadd v0, v0
    jump bb2
bb2:
    v4 = iadd v3, v0
    return v4
}
"#;
        let f = crate::mir::parse_func(src).expect(src);
        f.verify().unwrap();
        let err = f.verify_dominance().unwrap_err();
        assert!(err.contains("reads v3"), "{err}");
        let ok = crate::mir::parse_func(&src.replace("v4 = iadd v3, v0", "v4 = iadd v0, v0")).unwrap();
        ok.verify_dominance().unwrap();
    }
}
