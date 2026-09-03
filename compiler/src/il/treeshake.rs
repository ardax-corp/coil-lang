//! Reachability prune of unused function bodies before IL lower.
//!
//! Builtin dict thunks and user fns are eagerly emitted into the flat
//! [`CodeBuf`]. This pass keeps only functions reachable from roots (`main`,
//! optional test cases) via `Entry` / entry-label `Jump` / absolute call-like
//! `Byte`s, then splices dead emitting spans out of the buffer.

use std::collections::{HashMap, HashSet, VecDeque};

use common::Instruction;

use super::codebuf::CodeBuf;
use super::op::{IlOp, Label};
use super::opt::emitting_range_to_raw;

/// Roots and compiler tables needed to prune unused function bodies.
pub struct TreeshakeInput<'a> {
    /// Function name → emitting entry PC.
    pub functions: &'a mut HashMap<String, usize>,
    /// Function name → entry label.
    pub fn_entry_labels: &'a mut HashMap<String, Label>,
    /// Per-fn debug locals (dropped with dead names).
    pub fn_debug_locals: &'a mut HashMap<String, HashMap<String, u32>>,
    /// `(desc, entry_pc)` test harness cases; filtered when their fn dies.
    pub test_cases: &'a mut Vec<(String, u32)>,
    /// Always-live function names (`main`, …).
    pub root_names: &'a [String],
    /// When true, every `test_cases` entry PC is also a root.
    pub include_tests: bool,
    /// Start of setup / static-init / JMP-to-main (after builtin thunks).
    /// Consecutive thunk spans are clamped so this interstitial is never deleted.
    pub preserve_emit_start: Option<usize>,
}

/// Remove unreachable function bodies from `buf`. Returns how many names were
/// dropped, plus `(threshold, delta)` shrink events applied to entry PCs (so
/// callers can update parallel maps like `mono_offsets`).
pub fn prune_unused_functions(
    buf: &mut CodeBuf,
    input: TreeshakeInput<'_>,
) -> (usize, Vec<(usize, usize)>) {
    if input.functions.is_empty() {
        return (0, Vec::new());
    }

    let code_len = buf.len();
    let mut ordered: Vec<(usize, String)> = input
        .functions
        .iter()
        .map(|(n, &pc)| (pc, n.clone()))
        .collect();
    ordered.sort_by_key(|(pc, _)| *pc);

    let mut spans: HashMap<String, (usize, usize)> = HashMap::with_capacity(ordered.len());
    // Prefer exact body spans recorded at emit (user fns / mono clones).
    for f in buf.funcs() {
        if f.code_start < f.code_end && input.functions.contains_key(&f.name) {
            spans.insert(f.name.clone(), (f.code_start, f.code_end));
        }
    }
    for i in 0..ordered.len() {
        let name = &ordered[i].1;
        if spans.contains_key(name) {
            continue;
        }
        let start = ordered[i].0;
        let mut end = if i + 1 < ordered.len() {
            ordered[i + 1].0
        } else {
            code_len
        };
        // Builtin thunks are packed before setup; do not extend a thunk span
        // through static-init / JMP-to-main into the next user function.
        if let Some(preserve) = input.preserve_emit_start {
            if start < preserve && end > preserve {
                end = preserve;
            }
        }
        if start < end {
            spans.insert(name.clone(), (start, end));
        }
    }

    let label_to_name: HashMap<u32, String> = input
        .fn_entry_labels
        .iter()
        .map(|(n, l)| (l.0, n.clone()))
        .collect();

    // Emitting PC → function name for absolute CALL/CodePtr operands.
    let mut pc_to_name: HashMap<usize, String> = HashMap::new();
    for (name, &(start, _)) in &spans {
        pc_to_name.insert(start, name.clone());
    }
    for (pc, label) in buf.entry_labels() {
        if let Some(name) = label_to_name.get(&label.0) {
            pc_to_name.entry(pc).or_insert_with(|| name.clone());
        }
    }

    let mut live: HashSet<String> = HashSet::new();
    let mut work: VecDeque<String> = VecDeque::new();
    for name in input.root_names {
        if input.functions.contains_key(name) {
            live.insert(name.clone());
            work.push_back(name.clone());
        }
    }
    if input.include_tests {
        for &(_, pc) in input.test_cases.iter() {
            if let Some(name) = pc_to_name.get(&(pc as usize)) {
                if live.insert(name.clone()) {
                    work.push_back(name.clone());
                }
            }
        }
    }

    let ops = buf.ops();
    // Prologue + static-init / JMP-to-main before the first live root body.
    // Do not scan packed builtin thunks as if they were setup (that keeps
    // incidental callees live and defeats shaking).
    let setup_end = live
        .iter()
        .filter_map(|n| spans.get(n).map(|&(s, _)| s))
        .min()
        .unwrap_or(0);
    let setup_start = input.preserve_emit_start.unwrap_or(0).min(setup_end);
    if setup_end > setup_start {
        let (raw_s, raw_e) = emitting_range_to_raw(ops, setup_start, setup_end);
        for op in &ops[raw_s..raw_e] {
            for target in entry_targets(op) {
                if let Some(callee) = resolve_target(&target, &label_to_name, &pc_to_name) {
                    if live.insert(callee.clone()) {
                        work.push_back(callee);
                    }
                }
            }
        }
    }
    // Absolute JMP in the prologue may still name setup / main by PC.
    if setup_start > 0 {
        let (raw_s, raw_e) = emitting_range_to_raw(ops, 0, setup_start.min(3));
        for op in &ops[raw_s..raw_e] {
            for target in entry_targets(op) {
                if let Some(callee) = resolve_target(&target, &label_to_name, &pc_to_name) {
                    if live.insert(callee.clone()) {
                        work.push_back(callee);
                    }
                }
            }
        }
    }

    while let Some(name) = work.pop_front() {
        let Some(&(start, end)) = spans.get(&name) else {
            continue;
        };
        let (raw_s, raw_e) = emitting_range_to_raw(ops, start, end);
        for op in &ops[raw_s..raw_e] {
            for target in entry_targets(op) {
                if let Some(callee) = resolve_target(&target, &label_to_name, &pc_to_name) {
                    if live.insert(callee.clone()) {
                        work.push_back(callee);
                    }
                }
            }
        }
    }

    let dead: Vec<String> = input
        .functions
        .keys()
        .filter(|n| !live.contains(*n))
        .cloned()
        .collect();
    if dead.is_empty() {
        return (0, Vec::new());
    }

    let mut dead_spans: Vec<(usize, usize, String)> = dead
        .iter()
        .filter_map(|n| spans.get(n).map(|&(s, e)| (s, e, n.clone())))
        .collect();
    // Never delete the setup / static-init / JMP-to-main gap, even if a
    // consecutive thunk span was computed too wide.
    if let Some(preserve) = input.preserve_emit_start {
        let live_start = live
            .iter()
            .filter_map(|n| spans.get(n).map(|&(s, _)| s))
            .filter(|&s| s >= preserve)
            .min()
            .unwrap_or(preserve);
        if live_start > preserve {
            dead_spans = dead_spans
                .into_iter()
                .flat_map(|(s, e, n)| {
                    // Subtract [preserve, live_start) from [s, e).
                    let mut out = Vec::new();
                    if s < preserve {
                        out.push((s, e.min(preserve), n.clone()));
                    }
                    if e > live_start {
                        out.push((s.max(live_start), e, n));
                    }
                    out.into_iter().filter(|(a, b, _)| a < b)
                })
                .collect();
        }
    }
    dead_spans.sort_by_key(|(s, _, _)| *s);
    dead_spans.reverse(); // high-to-low so offsets stay valid

    let mut shrinks: Vec<(usize, usize)> = Vec::new();
    for (start, end, _name) in &dead_spans {
        remove_emitting_span(buf, *start, *end);
        let delta = end - start;
        shrink_pcs_after(input.functions, *end, delta);
        for (_, pc) in input.test_cases.iter_mut() {
            if (*pc as usize) >= *end {
                *pc -= delta as u32;
            }
        }
        shrinks.push((*end, delta));
    }

    let dropped = dead.len();
    for name in &dead {
        input.functions.remove(name);
        input.fn_entry_labels.remove(name);
        input.fn_debug_locals.remove(name);
    }
    let live_pcs: HashSet<usize> = input.functions.values().copied().collect();
    input
        .test_cases
        .retain(|(_, pc)| live_pcs.contains(&(*pc as usize)));

    // Keep surviving IlFunc metadata (especially `entry_sp` for scoped opts).
    // Spans were already shrunk / dead records dropped during deletes.
    let live_names: HashSet<&str> = input.functions.keys().map(|s| s.as_str()).collect();
    buf.retain_funcs(|f| live_names.contains(f.name.as_str()));
    (dropped, shrinks)
}

enum Target {
    Label(u32),
    Pc(usize),
}

fn entry_targets(op: &IlOp) -> Vec<Target> {
    match op {
        IlOp::Entry { target, .. } => vec![Target::Label(target.0)],
        IlOp::Jump { target, .. } => vec![Target::Label(target.0)],
        IlOp::Byte { byte, .. } => absolute_call_targets(byte),
        other => other
            .as_encode_byte()
            .map(|b| absolute_call_targets(&b))
            .unwrap_or_default(),
    }
}

fn absolute_call_targets(byte: &common::Byte) -> Vec<Target> {
    match *byte.bytecode() {
        Instruction::CALL | Instruction::TailCall | Instruction::MakeCoro => {
            let (_arity, pc) = byte.call_parts();
            vec![Target::Pc(pc as usize)]
        }
        Instruction::CodePtr | Instruction::MakePolyFn => {
            vec![Target::Pc(byte.operand_u32() as usize)]
        }
        _ => Vec::new(),
    }
}

fn resolve_target(
    target: &Target,
    label_to_name: &HashMap<u32, String>,
    pc_to_name: &HashMap<usize, String>,
) -> Option<String> {
    match target {
        Target::Label(id) => label_to_name.get(id).cloned(),
        Target::Pc(pc) => pc_to_name.get(pc).cloned(),
    }
}

fn shrink_pcs_after(map: &mut HashMap<String, usize>, threshold: usize, delta: usize) {
    if delta == 0 {
        return;
    }
    for pc in map.values_mut() {
        if *pc >= threshold {
            *pc -= delta;
        }
    }
}

fn remove_emitting_span(buf: &mut CodeBuf, emit_start: usize, emit_end: usize) {
    if emit_start >= emit_end {
        return;
    }
    let (raw_s, raw_e) = emitting_range_to_raw(buf.ops(), emit_start, emit_end);
    if raw_s >= raw_e {
        return;
    }
    // Drop records for the deleted span before shifting survivors; otherwise
    // survivors that slide down into `[start, end)` look like overlaps and
    // get removed.
    buf.remove_func_spans_overlapping(emit_start, emit_end);
    buf.remove_raw_range(raw_s, raw_e);
    buf.shift_entry_pcs_after_delete(emit_start, emit_end);
    buf.shrink_func_spans_after_delete(emit_start, emit_end);
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::op::EntryKind;
    use common::DebugLoc;
    use std::collections::HashMap;

    #[test]
    fn prunes_unreferenced_thunk_keeps_main() {
        let mut buf = CodeBuf::new();
        let mut functions = HashMap::new();
        let mut labels = HashMap::new();

        let dead_l = buf.bind_fresh_entry();
        let dead_pc = 0usize;
        functions.insert("Hash__unit__hash".into(), dead_pc);
        labels.insert("Hash__unit__hash".into(), dead_l);
        buf.push_const(0);
        buf.push_return();

        let main_pc = buf.len();
        let main_l = buf.bind_fresh_entry();
        functions.insert("main".into(), main_pc);
        labels.insert("main".into(), main_l);
        buf.push_const(1);
        buf.push_return();

        let mut locals = HashMap::new();
        let mut tests = Vec::new();
        let roots = vec!["main".into()];
        let (dropped, _) = prune_unused_functions(
            &mut buf,
            TreeshakeInput {
                functions: &mut functions,
                fn_entry_labels: &mut labels,
                fn_debug_locals: &mut locals,
                test_cases: &mut tests,
                root_names: &roots,
                include_tests: false,
                preserve_emit_start: None,
            },
        );
        assert_eq!(dropped, 1);
        assert!(functions.contains_key("main"));
        assert!(!functions.contains_key("Hash__unit__hash"));
        assert_eq!(functions["main"], 0);
        assert_eq!(buf.len(), 2); // CONST; RETURN
    }

    #[test]
    fn keeps_callee_of_main() {
        let mut buf = CodeBuf::new();
        let mut functions = HashMap::new();
        let mut labels = HashMap::new();

        let foo_pc = buf.len();
        let foo_l = buf.bind_fresh_entry();
        functions.insert("foo".into(), foo_pc);
        labels.insert("foo".into(), foo_l);
        buf.push_const(7);
        buf.push_return();

        let main_pc = buf.len();
        let main_l = buf.bind_fresh_entry();
        functions.insert("main".into(), main_pc);
        labels.insert("main".into(), main_l);
        buf.push_op(IlOp::Entry {
            kind: EntryKind::Call,
            arity: 0,
            target: foo_l,
            loc: DebugLoc::unknown(),
        ret_words: 1,
        });
        buf.push_pop();
        buf.push_return();

        let mut locals = HashMap::new();
        let mut tests = Vec::new();
        let roots = vec!["main".into()];
        let (dropped, _) = prune_unused_functions(
            &mut buf,
            TreeshakeInput {
                functions: &mut functions,
                fn_entry_labels: &mut labels,
                fn_debug_locals: &mut locals,
                test_cases: &mut tests,
                root_names: &roots,
                include_tests: false,
                preserve_emit_start: None,
            },
        );
        assert_eq!(dropped, 0);
        assert!(functions.contains_key("foo"));
        assert!(functions.contains_key("main"));
    }

    #[test]
    fn preserves_static_init_gap_between_thunk_and_main() {
        let mut buf = CodeBuf::new();
        let mut functions = HashMap::new();
        let mut labels = HashMap::new();

        // Packed builtin thunk; without preserve_emit_start its span would
        // swallow the interstitial setup ops through main's entry PC.
        let dead_l = buf.bind_fresh_entry();
        functions.insert("Hash__unit__hash".into(), 0usize);
        labels.insert("Hash__unit__hash".into(), dead_l);
        buf.push_const(0);
        buf.push_return();

        let preserve = buf.len();
        buf.push_const(11); // static-init / setup payload that must survive
        buf.push_pop();

        let main_pc = buf.len();
        let main_l = buf.bind_fresh_entry();
        functions.insert("main".into(), main_pc);
        labels.insert("main".into(), main_l);
        buf.push_const(1);
        buf.push_return();

        let mut locals = HashMap::new();
        let mut tests = Vec::new();
        let roots = vec!["main".into()];
        let (dropped, _) = prune_unused_functions(
            &mut buf,
            TreeshakeInput {
                functions: &mut functions,
                fn_entry_labels: &mut labels,
                fn_debug_locals: &mut locals,
                test_cases: &mut tests,
                root_names: &roots,
                include_tests: false,
                preserve_emit_start: Some(preserve),
            },
        );
        assert_eq!(dropped, 1);
        assert!(!functions.contains_key("Hash__unit__hash"));
        assert!(functions.contains_key("main"));
        assert!(
            buf.ops()
                .iter()
                .any(|op| matches!(op, IlOp::Const { imm: 11, .. })),
            "static-init CONST must survive thunk shake"
        );
        assert_eq!(functions["main"], 2); // setup CONST;POP then main
    }

    #[test]
    fn include_tests_roots_test_case_fn() {
        let mut buf = CodeBuf::new();
        let mut functions = HashMap::new();
        let mut labels = HashMap::new();

        let test_pc = buf.len();
        let test_l = buf.bind_fresh_entry();
        functions.insert("test_add".into(), test_pc);
        labels.insert("test_add".into(), test_l);
        buf.push_const(3);
        buf.push_return();

        let main_pc = buf.len();
        let main_l = buf.bind_fresh_entry();
        functions.insert("main".into(), main_pc);
        labels.insert("main".into(), main_l);
        buf.push_const(1);
        buf.push_return();

        let mut locals = HashMap::new();
        let mut tests = vec![("add".into(), test_pc as u32)];
        let roots = vec!["main".into()];
        let (dropped, _) = prune_unused_functions(
            &mut buf,
            TreeshakeInput {
                functions: &mut functions,
                fn_entry_labels: &mut labels,
                fn_debug_locals: &mut locals,
                test_cases: &mut tests,
                root_names: &roots,
                include_tests: true,
                preserve_emit_start: None,
            },
        );
        assert_eq!(dropped, 0);
        assert!(functions.contains_key("test_add"));
        assert_eq!(tests.len(), 1);
    }

    #[test]
    fn absolute_call_byte_keeps_callee() {
        let mut buf = CodeBuf::new();
        let mut functions = HashMap::new();
        let mut labels = HashMap::new();

        let foo_pc = buf.len();
        let foo_l = buf.bind_fresh_entry();
        functions.insert("foo".into(), foo_pc);
        labels.insert("foo".into(), foo_l);
        buf.push_const(7);
        buf.push_return();

        let main_pc = buf.len();
        let main_l = buf.bind_fresh_entry();
        functions.insert("main".into(), main_pc);
        labels.insert("main".into(), main_l);
        buf.push_op(IlOp::byte(
            common::Byte::new(Instruction::CALL).with_call_packed(0, foo_pc as u32),
        ));
        buf.push_pop();
        buf.push_return();

        let mut locals = HashMap::new();
        let mut tests = Vec::new();
        let roots = vec!["main".into()];
        let (dropped, _) = prune_unused_functions(
            &mut buf,
            TreeshakeInput {
                functions: &mut functions,
                fn_entry_labels: &mut labels,
                fn_debug_locals: &mut locals,
                test_cases: &mut tests,
                root_names: &roots,
                include_tests: false,
                preserve_emit_start: None,
            },
        );
        assert_eq!(dropped, 0);
        assert!(functions.contains_key("foo"));
        assert!(functions.contains_key("main"));
    }

    #[test]
    fn retains_live_func_entry_sp_metadata() {
        let mut buf = CodeBuf::new();
        let mut functions = HashMap::new();
        let mut labels = HashMap::new();

        let dead_l = buf.bind_fresh_entry();
        functions.insert("dead".into(), 0usize);
        labels.insert("dead".into(), dead_l);
        buf.push_const(0);
        buf.push_return();
        buf.record_func_with_sp("dead", Some(dead_l), 0, 2, 1);

        let main_pc = buf.len();
        let main_l = buf.bind_fresh_entry();
        functions.insert("main".into(), main_pc);
        labels.insert("main".into(), main_l);
        buf.push_const(1);
        buf.push_return();
        buf.record_func_with_sp("main", Some(main_l), main_pc, main_pc + 2, 4);

        let mut locals = HashMap::new();
        let mut tests = Vec::new();
        let roots = vec!["main".into()];
        let _ = prune_unused_functions(
            &mut buf,
            TreeshakeInput {
                functions: &mut functions,
                fn_entry_labels: &mut labels,
                fn_debug_locals: &mut locals,
                test_cases: &mut tests,
                root_names: &roots,
                include_tests: false,
                preserve_emit_start: None,
            },
        );
        assert_eq!(buf.funcs().len(), 1);
        assert_eq!(buf.funcs()[0].name, "main");
        assert_eq!(buf.funcs()[0].entry_sp, 4);
    }
}
