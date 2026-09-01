//! Differential check of the static cursor model against the real VM.
//!
//! `il::tell` predicts the shared operand/local cursor per PC. Reading the
//! handlers is not enough to trust those rules — a wrong one is silent memory
//! corruption, not a failing test — so every prediction is diffed against the
//! cursor the VM actually had, recorded by `machine::cursor_trace`.
//!
//! Symbolic-IL tell (used by pre-lower opts) is diffed against bytecode tell
//! on the same corpus via lower's `pre_to_post` map (COI-80).
//!
//! The assertion is one-directional: wherever the model says `Known(v)`, every
//! observed cursor at that PC must be `v`. `Unknown` is always allowed, so the
//! test cannot be satisfied by giving up — `model_covers_most_of_the_corpus`
//! guards the coverage side.

use common::{Byte, FnDebugSym, Instruction};
use compiler::Pipeline;
use compiler::tell::{self, Tell};
use machine::{Machine, cursor_trace, reset_cursor_trace};

struct Compiled {
    bytecode: Vec<Byte>,
    pool: Vec<u64>,
    strings: Vec<String>,
    static_slots: u32,
    fn_symbols: Vec<FnDebugSym>,
    pipeline: Pipeline,
}

fn compile(path: &str) -> Compiled {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root");
    let src =
        std::fs::read_to_string(root.join(path)).unwrap_or_else(|e| panic!("read {path}: {e}"));
    let mut pipeline = Pipeline::new();
    pipeline.bind_workspace_language_roots();
    let (bytecode, pool) = pipeline
        .compile_src(&src)
        .unwrap_or_else(|_| panic!("compile failed: {path}"));
    let fn_symbols = pipeline.program_debug().fn_symbols;
    Compiled {
        bytecode,
        pool,
        strings: pipeline.strings().to_vec(),
        static_slots: pipeline.static_slot_count(),
        fn_symbols,
        pipeline,
    }
}

fn compile_retaining_il(path: &str) -> Compiled {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root");
    let src =
        std::fs::read_to_string(root.join(path)).unwrap_or_else(|e| panic!("read {path}: {e}"));
    let mut pipeline = Pipeline::new();
    pipeline.bind_workspace_language_roots();
    let (bytecode, pool) = pipeline
        .compile_src_retaining_il(&src)
        .unwrap_or_else(|_| panic!("compile failed: {path}"));
    let fn_symbols = pipeline.program_debug().fn_symbols;
    Compiled {
        bytecode,
        pool,
        strings: pipeline.strings().to_vec(),
        static_slots: pipeline.static_slot_count(),
        fn_symbols,
        pipeline,
    }
}

/// Run and return `(pc, frame_relative_cursor)` in dispatch order.
fn trace(c: &Compiled) -> Vec<(u32, u32)> {
    reset_cursor_trace();
    let mut machine = Machine::<256>::default();
    c.pipeline.wire_host_natives(&mut machine);
    machine.run_raw(&c.bytecode, &c.pool, &c.strings, c.static_slots);
    cursor_trace()
}

/// Inclusive-exclusive PC range of each function, in entry order.
fn fn_ranges(syms: &[FnDebugSym], code_len: usize) -> Vec<(String, usize, usize)> {
    let mut sorted: Vec<&FnDebugSym> = syms.iter().collect();
    sorted.sort_by_key(|s| s.entry_pc);
    let mut out = Vec::with_capacity(sorted.len());
    for (i, s) in sorted.iter().enumerate() {
        let end = sorted
            .get(i + 1)
            .map(|n| n.entry_pc as usize)
            .unwrap_or(code_len);
        out.push((s.name.clone(), s.entry_pc as usize, end));
    }
    out
}

struct Report {
    checked: usize,
    known: usize,
    mismatches: Vec<String>,
}

/// Diff the model against one program's trace.
///
/// Each function body is seeded with the cursor the VM had on entry rather than
/// a computed arity: the goal is to validate the *propagation* rules, and using
/// the observed entry value keeps a wrong seed from masking them.
fn check(path: &str) -> Report {
    let c = compile(path);
    let observed = trace(&c);
    let ranges = fn_ranges(&c.fn_symbols, c.bytecode.len());

    let mut entry_seen: std::collections::HashMap<usize, u32> = std::collections::HashMap::new();
    for (pc, cur) in &observed {
        entry_seen.entry(*pc as usize).or_insert(*cur);
    }

    let mut report = Report {
        checked: 0,
        known: 0,
        mismatches: Vec::new(),
    };

    for (name, start, end) in &ranges {
        let Some(&seed) = entry_seen.get(start) else {
            continue; // never called
        };
        let info = tell::analyze_at(&c.bytecode, &c.pool, *start, seed);
        for (pc, actual) in &observed {
            let pc = *pc as usize;
            if pc < *start || pc >= *end {
                continue;
            }
            report.checked += 1;
            if let Tell::Known(predicted) = info.tell_before(pc) {
                report.known += 1;
                if predicted != *actual {
                    let op = *c.bytecode[pc].bytecode();
                    if report.mismatches.len() < 12 {
                        report.mismatches.push(format!(
                            "{path} {name} pc={pc} {op:?}: model={predicted} actual={actual}"
                        ));
                    }
                }
            }
        }
    }
    report
}

fn entry_seeds(c: &Compiled) -> std::collections::HashMap<usize, u32> {
    let observed = trace(c);
    let mut entry_seen: std::collections::HashMap<usize, u32> = std::collections::HashMap::new();
    for (pc, cur) in &observed {
        entry_seen.entry(*pc as usize).or_insert(*cur);
    }
    entry_seen
}

fn check_il(path: &str) -> tell::IlTellDiff {
    let c = compile_retaining_il(path);
    let ranges = fn_ranges(&c.fn_symbols, c.bytecode.len());
    let seeds = entry_seeds(&c);
    c.pipeline
        .diff_il_tell_against_bytecode(&c.bytecode, &c.pool, &ranges, &seeds)
}

/// `compile_src_retaining_il` must keep post-opt IL without the `dissect` feature.
#[test]
fn compile_src_retaining_il_keeps_pre_lower_ops() {
    let mut pipeline = Pipeline::new();
    pipeline
        .compile_src_retaining_il("fn main() {}")
        .expect("compile");
    let n = pipeline
        .retained_cursor_il_len()
        .expect("IL snapshot missing");
    assert!(n > 0, "retained IL should include prologue + main");
}

/// Plain `compile_src` must not populate the cursor-IL snap (retain is opt-in).
#[test]
fn compile_src_without_retain_leaves_no_cursor_il() {
    let mut pipeline = Pipeline::new();
    pipeline.compile_src("fn main() {}").expect("compile");
    assert!(pipeline.retained_cursor_il_len().is_none());
}

const CORPUS: &[&str] = &[
    "examples/fib.hy",
    "examples/perf/nsieve.hy",
    "examples/perf/binary_trees.hy",
    "examples/perf/mandelbrot.hy",
    "examples/perf/tak.hy",
    "examples/perf/bool_guard.hy",
    "examples/perf/match_sum.hy",
    "examples/perf/field_hot.hy",
    "examples/perf/dict_hot.hy",
    "examples/perf/array_mut.hy",
    "examples/inline_wrapped_call.hy",
    "examples/option.hy",
    "examples/result.hy",
    "examples/tree.hy",
];

#[test]
fn tell_cursor_model_matches_vm() {
    let mut all: Vec<String> = Vec::new();
    for path in CORPUS {
        let r = check(path);
        all.extend(r.mismatches);
    }
    assert!(
        all.is_empty(),
        "cursor model disagreed with the VM ({} shown):\n{}",
        all.len(),
        all.join("\n")
    );
}

/// A model that answered `Unknown` everywhere would pass the diff above, so pin
/// the rule table's reach: nearly every *executed* instruction must have a
/// modelled cursor effect.
///
/// This is deliberately not a check on how often the absolute cursor resolves.
/// At a loop header the cursor genuinely differs between the first entry and the
/// back edge — later iterations have stored to higher slots — so `Unknown` there
/// is the correct answer, not a gap. Only `JumpIfMatch`'s taken edge is a real
/// limitation: the payload arity lives in the runtime enum, not the opcode.
#[test]
fn tell_model_covers_executed_opcodes() {
    let mut executed = 0usize;
    let mut modelled = 0usize;
    let mut gaps: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for path in CORPUS {
        let c = compile(path);
        for (pc, _) in trace(&c) {
            let b = &c.bytecode[pc as usize];
            executed += 1;
            if compiler::tell::is_modelled(b, &c.pool) {
                modelled += 1;
            } else {
                *gaps.entry(format!("{:?}", b.bytecode())).or_default() += 1;
            }
        }
    }
    assert!(executed > 0, "corpus produced no dispatches");
    let pct = 100.0 * modelled as f64 / executed as f64;
    let mut gaps: Vec<_> = gaps.into_iter().collect();
    gaps.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    assert!(
        pct >= 97.0,
        "cursor rules cover only {pct:.1}% of executed instructions; gaps: {gaps:?}"
    );
}

/// `Seek` is the one op that sets the cursor absolutely, and `Unpack` writes
/// match-binding slots — the shapes most likely to drift from the VM.
#[test]
fn tell_model_matches_vm_on_match_bindings() {
    let r = check("examples/perf/match_sum.hy");
    assert!(r.mismatches.is_empty(), "{}", r.mismatches.join("\n"));
    let c = compile("examples/perf/match_sum.hy");
    assert!(
        c.bytecode
            .iter()
            .any(|b| matches!(*b.bytecode(), Instruction::Unpack | Instruction::Seek)),
        "expected the match path to emit Unpack / Seek"
    );
}

/// Pin the documented `JumpIfMatch` gap and that fused store floors stay modelled
/// on real codegen — the shapes a later copy-prop pass will delete against.
#[test]
fn tell_model_jump_if_match_gap_and_fused_store_coverage() {
    let mut saw_jim = false;
    let mut saw_fused_store = false;
    for path in CORPUS {
        let c = compile(path);
        let r = check(path);
        assert!(
            r.mismatches.is_empty(),
            "{path}: {}",
            r.mismatches.join("\n")
        );
        for b in &c.bytecode {
            match *b.bytecode() {
                Instruction::JumpIfMatch => {
                    saw_jim = true;
                    assert!(
                        !tell::is_modelled(b, &c.pool),
                        "{path}: JumpIfMatch must stay unmodelled (runtime arity)"
                    );
                }
                Instruction::BinSlotImmStore | Instruction::BinSlotSlotStore => {
                    saw_fused_store = true;
                    assert!(
                        tell::is_modelled(b, &c.pool),
                        "{path}: fused store must be modelled: {:?}",
                        b.bytecode()
                    );
                }
                _ => {}
            }
        }
    }
    assert!(saw_jim, "corpus must exercise JumpIfMatch");
    assert!(
        saw_fused_store,
        "corpus must exercise BinSlotImmStore / BinSlotSlotStore"
    );
}

/// Symbolic-IL tell vs bytecode tell on the same corpus (COI-80).
///
/// Seeded with the VM entry cursor per function, same as the bytecode/VM gate.
/// Fail-closed: IL `Known(v)` must equal bytecode `Known(v)` at the mapped PC.
#[test]
fn tell_symbolic_il_matches_bytecode() {
    let mut all: Vec<String> = Vec::new();
    let mut known = 0usize;
    let mut saw_call = false;
    let mut saw_store = false;
    for path in CORPUS {
        let r = check_il(path);
        all.extend(r.mismatches.into_iter().map(|m| format!("{path} {m}")));
        known += r.known;
        saw_call |= r.saw_call;
        saw_store |= r.saw_store;
    }
    assert!(
        all.is_empty(),
        "symbolic-IL tell disagreed with bytecode ({} shown):\n{}",
        all.len(),
        all.join("\n")
    );
    assert!(
        known > 0,
        "IL gate compared no Known cursors — mapping is vacuous"
    );
    assert!(saw_call, "corpus must exercise Entry{{Call}}");
    assert!(saw_store, "corpus must exercise StorePop");
}

/// COI-80 regression: `effect_il` once gave `Entry{{Call}}` JumpIfMatch's
/// `arity - 1` instead of `call_arity_delta` (`1 - arity`). Arity 0 is the
/// sharpest split (CALL pushes a result; the JumpIfMatch rule would pop).
#[test]
fn tell_symbolic_il_entry_call_delta_is_not_jump_if_match_arity_minus_one() {
    for arity in [0u32, 2, 3] {
        let seed = arity + 2;
        let (il, bc) = tell::entry_call_tell_after(arity, seed);
        let call = seed.saturating_add_signed(1 - arity as i32);
        let jim = seed.saturating_add_signed(arity as i32 - 1);
        assert_eq!(
            bc,
            Tell::Known(call),
            "bytecode CALL arity {arity} seed {seed}"
        );
        assert_eq!(il, bc, "IL Entry{{Call}} arity {arity} must match CALL");
        assert_ne!(
            il,
            Tell::Known(jim),
            "Entry{{Call}} arity {arity} must not use JumpIfMatch's arity-1 (would be {jim})"
        );
    }
    let (il, bc) = tell::store_pop_tell_after(5, 0);
    assert_eq!(il, bc);
    assert_eq!(il, Tell::Known(6));
}
