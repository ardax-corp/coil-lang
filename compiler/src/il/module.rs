//! Per-function IL module: owning view rebuilt at finalize from flat emit.
//!
//! Cheap split of one [`super::CodeBuf`] — not a second IL language.
//! Codegen keeps a flat [`super::CodeBuf`] stream. At lower time the buffer is
//! split into owned function bodies (plus prologue / glue / epilogue), opts
//! run per body, then the stream is concatenated for a single fuse/PC lower.

use std::collections::{HashMap, HashSet};

use super::func::IlFunc;
use super::op::{IlJumpKind, IlOp, Label};
use super::opt::{self, OptimizeOptions};

type FlatIl = (Vec<IlOp>, HashMap<u32, u32>, Vec<HashMap<u32, u32>>);

/// One function's owned IL ops (labels inclusive at span edges).
#[derive(Clone)]
pub struct IlFuncBody {
    /// Span / entry metadata from emit-time [`IlFunc`].
    pub meta: IlFunc,
    pub ops: Vec<IlOp>,
}

/// Flat stream partitioned into prologue, function bodies, and glue.
///
/// Rebuilt at finalize; bodies are the source of truth for per-func opts
/// until [`Self::optimize_and_flatten`] concatenates for lower.
#[derive(Clone, Default)]
pub struct IlModule {
    pub prologue: Vec<IlOp>,
    pub funcs: Vec<IlFuncBody>,
    /// Gap after `funcs[i]` (before the next func or epilogue).
    pub glue: Vec<Vec<IlOp>>,
    pub epilogue: Vec<IlOp>,
    /// Logical emitting PC → entry label (copied from [`super::CodeBuf`] at finalize).
    ///
    /// CALL/CodePtr rewrite to `IlOp::Entry` happens at emit time on `CodeBuf`;
    /// this map is retained for diagnostics and future module-level remapping.
    pub entry_at_offset: HashMap<usize, Label>,
    /// S2b drafts filled during [`Self::optimize_and_flatten`].
    pub stack_map_drafts: Vec<crate::mir::DraftFrameMap>,
    /// I7 / C3 deopt resume drafts (compiler-internal; not archived).
    pub deopt_map_drafts: Vec<crate::mir::DraftDeoptMap>,
    /// Dense bodies whose frames need a decoded extent (no S2b maps).
    pub needs_frame_extent: HashSet<String>,
    /// Original IL slot → reconstruct slot after dense / LIR (named lets).
    pub debug_slot_remaps: HashMap<String, HashMap<u32, u32>>,
}

impl IlModule {
    /// Split a flat op buffer using emitting spans from `funcs`.
    ///
    /// `glue[i]` is the gap after `funcs[i]` (before the next func or epilogue).
    /// Empty `funcs` yields the whole buffer as prologue.
    pub fn from_flat(ops: &[IlOp], funcs: &[IlFunc]) -> Self {
        if funcs.is_empty() {
            return Self {
                prologue: ops.to_vec(),
                funcs: Vec::new(),
                glue: Vec::new(),
                epilogue: Vec::new(),
                entry_at_offset: HashMap::new(),
                stack_map_drafts: Vec::new(),
                deopt_map_drafts: Vec::new(),
                needs_frame_extent: HashSet::new(),
                debug_slot_remaps: HashMap::new(),
            };
        }

        let mut ranges: Vec<(usize, usize, usize)> = funcs
            .iter()
            .enumerate()
            .filter(|(_, f)| f.code_start < f.code_end)
            .map(|(i, f)| {
                let (s, e) = opt::emitting_range_to_raw(ops, f.code_start, f.code_end);
                (i, s, e)
            })
            .filter(|(_, s, e)| s < e)
            .collect();
        ranges.sort_by_key(|&(_, s, _)| s);

        let mut module = Self::default();
        let mut cursor = 0usize;
        for (fi, raw_start, raw_end) in &ranges {
            if cursor < *raw_start {
                let gap = ops[cursor..*raw_start].to_vec();
                if module.funcs.is_empty() {
                    module.prologue = gap;
                } else {
                    module.glue.push(gap);
                }
            } else if !module.funcs.is_empty() {
                module.glue.push(Vec::new());
            }
            module.funcs.push(IlFuncBody {
                meta: funcs[*fi].clone(),
                ops: ops[*raw_start..*raw_end].to_vec(),
            });
            cursor = *raw_end;
        }
        while module.glue.len() + 1 < module.funcs.len() {
            module.glue.push(Vec::new());
        }
        if cursor < ops.len() {
            module.epilogue = ops[cursor..].to_vec();
        }
        module
    }

    /// Attach entry-label map from the emit-time [`super::CodeBuf`].
    pub fn with_entries(mut self, entry_at_offset: HashMap<usize, Label>) -> Self {
        self.entry_at_offset = entry_at_offset;
        self
    }

    /// Concatenate prologue / bodies / glue / epilogue into one op stream.
    ///
    /// Function bodies (and trailing glue) keep a private label namespace during
    /// per-func opts; remap on concat so lower never binds a jump to another
    /// function's label with the same numeric id. Prologue/epilogue are copied
    /// verbatim; cross-function `Jump`/`Entry` targets are patched per segment.
    pub fn to_flat(&self) -> FlatIl {
        let mut module = self.clone();
        absorb_trailing_labels(&mut module);
        let mut out = Vec::new();
        // New ids must not overlap old Label/Jump/Entry ids still sitting on
        // cross-function CALL sites until the post-concat patch.
        let mut next_label = module.max_code_label().saturating_add(1);
        let mut prior_labels = HashMap::new();
        let mut entry_labels = HashMap::new();
        let mut func_label_maps = Vec::new();
        // Old label ids bound in each body+glue chunk (before remap).
        let mut chunk_bound: Vec<std::collections::HashSet<u32>> = Vec::new();
        let mut segment_ranges: Vec<(usize, usize)> = Vec::new();
        if !self.prologue.is_empty() {
            let start = out.len();
            out.extend(self.prologue.iter().cloned());
            segment_ranges.push((start, out.len()));
        }
        for (i, body) in module.funcs.iter().enumerate() {
            let start = out.len();
            let mut chunk = body.ops.clone();
            if let Some(g) = module.glue.get(i) {
                chunk.extend(g.iter().cloned());
            }
            let old_entry = body.meta.entry.map(|Label(id)| id);
            chunk_bound.push(
                chunk
                    .iter()
                    .filter_map(|op| match op {
                        IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) => Some(*id),
                        _ => None,
                    })
                    .collect(),
            );
            let (chunk, map) = opt::remap_label_space(&chunk, &mut next_label, &prior_labels);
            // Prefer the remapped emit-time entry. If opts dropped that id
            // (preheader / relabel), CALL still has to land on this body.
            let new_entry = old_entry
                .and_then(|old| map.get(&old).copied())
                .or_else(|| first_label_id(&chunk));
            if let (Some(old), Some(new)) = (old_entry, new_entry) {
                entry_labels.insert(old, new);
            }
            // Bodies emitted without a recorded function (dictionary adapter
            // thunks) sit in glue, which per-body opts never relabel, so its
            // emit-time ids are the call targets. Another body's opts may
            // mint the same id, which makes the unique-map fallback refuse.
            if let Some(g) = module.glue.get(i) {
                for op in g {
                    if let IlOp::Label(Label(old)) = op
                        && let Some(&new) = map.get(old)
                    {
                        entry_labels.entry(*old).or_insert(new);
                    }
                }
            }
            func_label_maps.push(map.clone());
            merge_remap_labels(&mut prior_labels, map);
            out.extend(chunk);
            segment_ranges.push((start, out.len()));
        }
        if !module.epilogue.is_empty() {
            let start = out.len();
            out.extend(module.epilogue.iter().cloned());
            segment_ranges.push((start, out.len()));
        }
        // Function entry → flat id, taken from the chunk that binds the entry
        // label: the function's own body, else the previous chunk's glue.
        // `prior_labels` is first-wins over every body's private label space,
        // and per-body opts mint ids that may equal a later function's entry.
        let mut jump_entries: HashMap<u32, u32> = HashMap::new();
        for (k, body) in module.funcs.iter().enumerate() {
            let Some(Label(old)) = body.meta.entry else {
                continue;
            };
            let owner = if chunk_bound[k].contains(&old) {
                Some(k)
            } else if k > 0 && chunk_bound[k - 1].contains(&old) {
                Some(k - 1)
            } else {
                None
            };
            if let Some(new) = owner.and_then(|i| func_label_maps[i].get(&old).copied()) {
                jump_entries.insert(old, new);
            }
        }
        let flat_label_ids: std::collections::HashSet<u32> =
            prior_labels.values().copied().collect();
        for (idx, (start, end)) in segment_ranges.iter().copied().enumerate() {
            let is_prologue = idx == 0 && !self.prologue.is_empty();
            if is_prologue {
                remap_cross_function_entry_call_targets(
                    &mut out[start..end],
                    &func_label_maps,
                    &entry_labels,
                );
                remap_cross_function_jump_targets(
                    &mut out[start..end],
                    &prior_labels,
                    &jump_entries,
                    &flat_label_ids,
                );
            } else {
                remap_cross_function_jump_targets(
                    &mut out[start..end],
                    &prior_labels,
                    &jump_entries,
                    &flat_label_ids,
                );
                remap_cross_function_entry_call_targets(
                    &mut out[start..end],
                    &func_label_maps,
                    &entry_labels,
                );
            }
        }
        (out, prior_labels, func_label_maps)
    }

    fn max_code_label(&self) -> u32 {
        let mut max = opt::max_code_label(&self.prologue);
        for body in &self.funcs {
            max = max.max(opt::max_code_label(&body.ops));
        }
        for gap in &self.glue {
            max = max.max(opt::max_code_label(gap));
        }
        max.max(opt::max_code_label(&self.epilogue))
    }

    /// Loops whose body stores locals above the entry cursor reach their
    /// header with one cursor on entry and a higher one on the back edge, so
    /// precise frame maps only know a range there and stale loop words stay
    /// ambiguous GC roots. For an interpreted (fuse-IL) body, a
    /// `CONST 0; STORE b-1` preheader raises the entry cursor to the back-edge
    /// one `b` (from the tell analysis). It runs after IL optimization and
    /// MIR tiering, so neither sees it; dense / LIR bodies manage their own
    /// frame. Only loops that can reach a GC safepoint are touched, and only
    /// when the tell analysis confirms the entry cursor is below `b` (so
    /// slot `b-1` is no live local) and the header then has cursor `b` on
    /// every edge.
    fn apply_loop_cursor_raises(&mut self, tier: &[&str]) {
        for (i, body) in self.funcs.iter_mut().enumerate() {
            if tier.get(i).copied() != Some("fuse") {
                continue;
            }
            let entry_sp = body.meta.entry_sp;
            let mut done: HashSet<Label> = HashSet::new();
            while let Some(lp) = super::analysis::find_natural_loops(&body.ops)
                .into_iter()
                .find(|lp| !done.contains(&lp.header_label))
            {
                done.insert(lp.header_label);
                if !body.ops[lp.header..=lp.latch].iter().any(il_may_collect) {
                    continue;
                }
                let info = super::tell::analyze_il_at(&body.ops, entry_sp);
                if info.tell_before(lp.header).known().is_some() {
                    continue;
                }
                let Some(back) = loop_back_edge_tell(&body.ops, &lp, entry_sp) else {
                    continue;
                };
                let mut trial = body.ops.clone();
                let pre = Label(
                    trial
                        .iter()
                        .filter_map(|op| match op {
                            IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) => Some(*id),
                            IlOp::Jump { target: Label(id), .. } => Some(*id),
                            _ => None,
                        })
                        .max()
                        .unwrap_or(0)
                        + 1,
                );
                for (j, op) in trial.iter_mut().enumerate() {
                    if (lp.header..=lp.latch).contains(&j) {
                        continue;
                    }
                    if let IlOp::Jump { target, .. } = op
                        && *target == lp.header_label
                    {
                        *target = pre;
                    }
                }
                let loc = common::DebugLoc::unknown();
                trial.splice(
                    lp.header..lp.header,
                    [
                        IlOp::Label(pre),
                        IlOp::Const { imm: 0, loc },
                        IlOp::StorePop {
                            slot: back - 1,
                            loc,
                        },
                    ],
                );
                let check = super::tell::analyze_il_at(&trial, entry_sp);
                let entry_ok = check
                    .tell_before(lp.header + 1)
                    .known()
                    .is_some_and(|e| e < back);
                let header_ok = check.tell_before(lp.header + 3).known() == Some(back);
                if entry_ok && header_ok {
                    body.ops = trial;
                }
            }
        }
    }

    /// Per-func opts on each body, then concatenate the bodies.
    ///
    /// `pool` is the module const pool (`f64` / boxed int bits) for algebraic
    /// float identity / const-fold peeps (may push folded float results).
    pub fn optimize_and_flatten(
        &mut self,
        opts: &OptimizeOptions,
        pool: &mut Vec<u64>,
    ) -> FlatIl {
        if self.funcs.is_empty() {
            let (mut ops, remap, func_maps) = self.to_flat();
            opt::optimize(&mut ops, opts, pool);
            return (ops, remap, func_maps);
        }

        for body in self.funcs.iter_mut().filter(|b| !b.meta.pinned) {
            drop_jumps_to_next_label(&mut body.ops);
            opt::optimize_at(&mut body.ops, opts, body.meta.entry_sp as i32, pool);
        }

        // After stack-IL LICM/CSE so 4.0/2.0 live in the preheader.
        // Leaf-first: a caller may take dense once every callee it CALLs is dense.
        // I8: leftovers then try IL→MIR→LIR (`lir_eligible`); fuse-IL if
        // a LIR reconstruct wall hits. Q6–Q8 first rungs enter dense +
        // cost; they are not LIR walls.
        // S2d: snapshot maps from stack-IL before dense replace (dense residuals
        // cannot re-infer). Re-lift after LIR / fuse-IL when that succeeds.
        self.stack_map_drafts.clear();
        self.needs_frame_extent.clear();
        let mut pre_maps = std::collections::HashMap::<String, crate::mir::DraftFrameMap>::new();
        if opts.mir_specialize {
            for body in &self.funcs {
                if let Some(draft) = crate::mir::try_build_draft(
                    &body.ops,
                    &body.meta.name,
                    body.meta.entry_sp,
                    pool,
                    &body.meta.unboxed_fields,
                ) {
                    pre_maps.insert(body.meta.name.clone(), draft);
                }
            }
        }
        let mut dense_calls = crate::mir::DenseCallMap::new();
        // Pinned bodies stay fuse-IL as emitted.
        let mut pending: Vec<usize> = (0..self.funcs.len())
            .filter(|&i| !self.funcs[i].meta.pinned)
            .collect();
        let mut dense_why: Vec<Option<String>> = vec![None; self.funcs.len()];
        let mut lir_why: Vec<Option<String>> = vec![None; self.funcs.len()];
        let mut tier: Vec<&'static str> = vec!["fuse"; self.funcs.len()];
        let mut side_remaps = HashMap::<String, HashMap<u32, u32>>::new();
        let mut side_deopts = Vec::new();
        while !pending.is_empty() && opts.mir_specialize {
            let mut next = Vec::new();
            let mut progressed = false;
            for i in pending.iter().copied() {
                let body = &mut self.funcs[i];
                let mut side = crate::mir::BodySidecar::default();
                if let Some((dense, abi)) = crate::mir::try_specialize_body_side(
                    &body.ops,
                    &body.meta.name,
                    body.meta.entry_sp,
                    pool,
                    &dense_calls,
                    body.meta.entry,
                    &mut side,
                ) {
                    if drops_bound_label(&body.ops, &dense) {
                        // A `defer` thunk sits in its function's body and is
                        // reached only by `CALL`; the reconstruct keeps blocks
                        // reachable from the entry and loses it (#760).
                        dense_why[i] = Some("drops a called inner label".to_string());
                        next.push(i);
                        continue;
                    }
                    if !side.debug_slot_remap.is_empty() {
                        side_remaps.insert(body.meta.name.clone(), side.debug_slot_remap);
                    }
                    if let Some(deopt) = side.deopt {
                        side_deopts.push(deopt);
                    }
                    if side.needs_frame_extent {
                        self.needs_frame_extent.insert(body.meta.name.clone());
                    }
                    if let Some(crate::il::Label(id)) = body.meta.entry {
                        dense_calls.insert(id, abi.clone());
                    }
                    if let Some(id) = first_label_id(&dense) {
                        dense_calls.insert(id, abi);
                    }
                    body.ops = dense;
                    tier[i] = "dense";
                    progressed = true;
                } else {
                    dense_why[i] = crate::mir::take_dense_refusal();
                    next.push(i);
                }
            }
            if !progressed {
                break;
            }
            pending = next;
        }
        let pinned = self.funcs.iter().filter(|b| b.meta.pinned).count();
        let dense_kept = self.funcs.len() - pending.len() - pinned;
        let mut lir_kept = 0usize;
        for i in pending.iter().copied() {
            if !opts.mir_specialize {
                break;
            }
            let body = &mut self.funcs[i];
            let mut side = crate::mir::BodySidecar::default();
            if let Some(lir) = crate::mir::try_lower_abi_body_side(
                &body.ops,
                &body.meta.name,
                body.meta.entry_sp,
                pool,
                &body.meta.unboxed_fields,
                &mut side,
            ) {
                // Do not re-run stack-IL opts: `local_cse` refuses MOD and
                // rematerializes a stored remainder (pair_int_churn +12%).
                if drops_bound_label(&body.ops, &lir) {
                    lir_why[i] = Some("drops a called inner label".to_string());
                } else if lir_keeps(&body.ops, &lir) {
                    if !side.debug_slot_remap.is_empty() {
                        side_remaps.insert(body.meta.name.clone(), side.debug_slot_remap);
                    }
                    if let Some(deopt) = side.deopt {
                        side_deopts.push(deopt);
                    }
                    body.ops = lir;
                    tier[i] = "lir";
                    lir_kept += 1;
                } else {
                    lir_why[i] = Some("lir cost gate".to_string());
                }
            } else {
                lir_why[i] = Some(
                    crate::mir::take_lir_refusal().unwrap_or_else(|| "lir refused".to_string()),
                );
            }
            if opts.collect_stats && let Some(why) = &lir_why[i] {
                super::opt::note_fuse_reason(why);
            }
        }
        if opts.collect_stats {
            let (dense, fuse) = if opts.mir_specialize {
                (dense_kept, pending.len() - lir_kept + pinned)
            } else {
                (0, self.funcs.len())
            };
            super::opt::note_body_tiers(dense, lir_kept, fuse);
            for (i, body) in self.funcs.iter().enumerate() {
                super::opt::note_body_tier(super::opt::BodyTier {
                    name: body.meta.name.clone(),
                    tier: tier[i].to_string(),
                    dense_reason: dense_why[i].take(),
                    lir_reason: lir_why[i].take(),
                });
            }
        }
        self.debug_slot_remaps.extend(side_remaps);
        self.deopt_map_drafts.extend(side_deopts);

        // S2b: prefer a draft from the final body (fuse-IL / LIR). Dense
        // keep the pre-MIR snapshot so looping Make* still bind. Drafts see
        // bodies before the loop cursor raises (added just below).
        if opts.mir_specialize {
            for body in &self.funcs {
                if let Some(draft) = crate::mir::try_build_draft(
                    &body.ops,
                    &body.meta.name,
                    body.meta.entry_sp,
                    pool,
                    &body.meta.unboxed_fields,
                ) {
                    self.stack_map_drafts.push(draft);
                } else if let Some(draft) = pre_maps.remove(&body.meta.name) {
                    self.stack_map_drafts.push(draft);
                }
            }
        }

        if opts.mir_specialize {
            self.apply_loop_cursor_raises(&tier);
        } else {
            self.apply_loop_cursor_raises(&vec!["fuse"; self.funcs.len()]);
        }
        self.to_flat()
    }
}

/// Split + simulated MIR replace of the next body, then lower.
///
/// Used by the crate unit test and by `compiler/tests/coi407_trailing_label.rs`
/// so CI can run this under `--release` without compiling `lib.tests.rs`
/// (`Instruction: Debug` is debug_assertions-only).
pub(crate) fn prove_trailing_if_end_after_next_body_replace() {
    let loc = common::DebugLoc::unknown();
    let ops = vec![
        IlOp::Label(Label(1)),
        IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            target: Label(8),
            loc,
            hint: Default::default(),
        },
        IlOp::Return { loc, ret_words: 1 },
        IlOp::Label(Label(8)),
        IlOp::Label(Label(2)),
        IlOp::Const { imm: 0, loc },
        IlOp::Return { loc, ret_words: 1 },
    ];
    let funcs = vec![
        super::IlFunc::with_entry_sp("pred", Some(Label(1)), 0, 2, 0),
        super::IlFunc::with_entry_sp("hot", Some(Label(2)), 2, 4, 0),
    ];
    let mut m = IlModule::from_flat(&ops, &funcs);
    assert!(
        m.funcs[0]
            .ops
            .iter()
            .any(|op| matches!(op, IlOp::Label(Label(8))))
            || m.glue
                .first()
                .is_some_and(|g| g.iter().any(|op| matches!(op, IlOp::Label(Label(8))))),
        "pred or its trailing glue must keep end-label 8"
    );
    assert!(
        !m.funcs[1]
            .ops
            .iter()
            .any(|op| matches!(op, IlOp::Label(Label(8)))),
        "hot must not steal pred's trailing end-label"
    );
    m.funcs[1].ops = vec![
        IlOp::Label(Label(2)),
        IlOp::Const { imm: 0, loc },
        IlOp::Return { loc, ret_words: 1 },
    ];
    let (flat, _, _) = m.to_flat();
    let jmp = flat
        .iter()
        .find_map(|op| match op {
            IlOp::Jump {
                target,
                kind: IlJumpKind::JumpIfFalse,
                ..
            } => Some(target.0),
            _ => None,
        })
        .expect("pred JMPF");
    let bound: Vec<u32> = flat
        .iter()
        .filter_map(|op| match op {
            IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) => Some(*id),
            _ => None,
        })
        .collect();
    assert!(
        bound.contains(&jmp),
        "JMPF target {jmp} must be bound, bound={bound:?}"
    );
    let mut buf = super::CodeBuf::new();
    for op in &flat {
        buf.push_op(op.clone());
    }
    buf.lower_in_place(&mut Vec::new());
}

/// Emitting-op cost for MIR→LIR replace: refuse a reconstruct that grew
/// the body (naive slot spill). Labels are free. `Seek` / `StorePop` are
/// expensive so leftover lets keep fuse-IL (ConstReturnImm).
/// Keep a MIR→LIR reconstruct when it is no costlier than the opted fuse-IL
/// (+ match slack). Costs weight loop bodies (×8 per nesting level) so a
/// smaller hot loop can win against a larger cold tail — a flat count kept
/// fuse-IL on churn `main`s whose LIR loop was faster. Weighting needs both
/// sides to expose the same loops; otherwise compare flat counts.
/// A weighted tie is settled by the flat count.
/// Drop `JMP L` when `L` is the next label: it is a fall-through. Passes that
/// turn stack words into slots misread the words such a jump carries (an
/// `Unpack` payload went to the wrong slots, #771).
fn drop_jumps_to_next_label(ops: &mut Vec<IlOp>) {
    let mut i = 0;
    while i + 1 < ops.len() {
        let next = match &ops[i] {
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target,
                ..
            } => ops[i + 1..]
                .iter()
                .take_while(|op| matches!(op, IlOp::Label(_) | IlOp::JoinLabel(_)))
                .any(|op| matches!(op, IlOp::Label(l) | IlOp::JoinLabel(l) if l == target)),
            _ => false,
        };
        if next {
            ops.remove(i);
        } else {
            i += 1;
        }
    }
}

/// `new` still jumps to or calls a label `old` bound, but no longer binds it.
fn drops_bound_label(old: &[IlOp], new: &[IlOp]) -> bool {
    let bound = |ops: &[IlOp]| -> std::collections::HashSet<u32> {
        ops.iter()
            .filter_map(|op| match op {
                IlOp::Label(l) | IlOp::JoinLabel(l) => Some(l.0),
                _ => None,
            })
            .collect()
    };
    let (was, now) = (bound(old), bound(new));
    new.iter().any(|op| match op {
        IlOp::Entry { target, .. } | IlOp::Jump { target, .. } => was.contains(&target.0) && !now.contains(&target.0),
        _ => false,
    })
}

fn lir_keeps(fuse_ops: &[IlOp], lir: &[IlOp]) -> bool {
    let fuse_loops = super::analysis::find_natural_loops(fuse_ops);
    let lir_loops = super::analysis::find_natural_loops(lir);
    let slack = lir_cost_slack(fuse_ops);
    let flat_ok = lir_emit_cost(lir, &[]) <= lir_emit_cost(fuse_ops, &[]).saturating_add(slack);
    if fuse_loops.is_empty() || fuse_loops.len() != lir_loops.len() {
        return flat_ok;
    }
    let f = lir_emit_cost(fuse_ops, &fuse_loops);
    let l = lir_emit_cost(lir, &lir_loops);
    // IL-level cost cannot see fuse-select packing; a weighted tie goes to
    // the flat count, which still favours the tighter fuse-IL loop.
    l < f || (l <= f.saturating_add(slack) && flat_ok)
}

/// Static reconstruct cost; ops inside `loops` weigh ×8 per level (max 2).
fn lir_emit_cost(ops: &[IlOp], loops: &[super::analysis::NaturalLoop]) -> usize {
    ops.iter()
        .enumerate()
        .filter(|(_, op)| !matches!(op, IlOp::Label(_) | IlOp::JoinLabel(_)))
        .map(|(i, op)| {
            let base = match op {
                IlOp::StorePop { .. } => 2,
                IlOp::Byte { byte, .. }
                    if matches!(*byte.bytecode(), common::Instruction::Seek) =>
                {
                    2 + (byte.operand_u32() as usize) / 16
                }
                _ => 1,
            };
            let depth = loops
                .iter()
                .filter(|lp| lp.header <= i && i <= lp.latch)
                .count()
                .min(2) as u32;
            base * 8usize.pow(depth)
        })
        .sum()
}

/// Runtime-neutral slack for I2 match / two-slot construct reconstructs
/// that add a frame `Seek` the opted fuse-IL never emitted.
fn lir_cost_slack(ops: &[IlOp]) -> usize {
    let mut slack = 0usize;
    for op in ops {
        match op {
            IlOp::Return { ret_words, .. } if *ret_words >= 2 => slack = slack.max(3),
            IlOp::Jump {
                kind: IlJumpKind::JumpIfMatch { .. },
                ..
            } => slack = slack.max(3),
            IlOp::Byte { byte, .. } if *byte.bytecode() == common::Instruction::Unpack => {
                slack = slack.max(3);
            }
            _ => {}
        }
    }
    slack
}

/// Trailing `if { raise }` end-labels sit in glue / epilogue
/// (`emitting_range_to_raw` stops at the last code op and does not steal
/// them into the next function). Attach only those binds that the last
/// function jumps to and does not already define, so remap stays local
/// and does not steal another function's entry (finalizer / static-init
/// prologue).
fn absorb_trailing_labels(module: &mut IlModule) {
    let Some(last) = module.funcs.last_mut() else {
        return;
    };
    use std::collections::HashSet;
    let defined: HashSet<u32> = last
        .ops
        .iter()
        .filter_map(|op| match op {
            IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) => Some(*id),
            _ => None,
        })
        .collect();
    let needed: HashSet<u32> = last
        .ops
        .iter()
        .filter_map(|op| match op {
            IlOp::Jump { target, .. } => Some(target.0),
            _ => None,
        })
        .filter(|id| !defined.contains(id))
        .collect();
    if needed.is_empty() {
        return;
    }
    let mut kept = Vec::new();
    let mut moved = Vec::new();
    for op in module.epilogue.drain(..) {
        match op {
            IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) if needed.contains(&id) => {
                moved.push(op);
            }
            other => kept.push(other),
        }
    }
    last.ops.extend(moved);
    module.epilogue = kept;
}

fn merge_remap_labels(prior: &mut HashMap<u32, u32>, local: HashMap<u32, u32>) {
    for (old, new) in local {
        prior.entry(old).or_insert(new);
    }
}

fn first_label_id(ops: &[IlOp]) -> Option<u32> {
    ops.iter().find_map(|op| match op {
        IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) => Some(*id),
        _ => None,
    })
}

/// Prefer recorded function entries so a later body's internal label cannot
/// steal a reminted callee entry (unique-hit false positive). Unique old ids
/// still map 1:1 for typeclass / default-method CALLs that are not `IlFunc.entry`.
fn resolve_cross_function_entry(
    old: u32,
    maps: &[HashMap<u32, u32>],
    entry_labels: &HashMap<u32, u32>,
) -> Option<u32> {
    if let Some(&entry) = entry_labels.get(&old) {
        return Some(entry);
    }
    let mut uniq = None;
    let mut hits = 0u8;
    for map in maps {
        if let Some(&new) = map.get(&old) {
            hits = hits.saturating_add(1);
            uniq = Some(new);
            if hits > 1 {
                return None;
            }
        }
    }
    uniq
}

fn remap_cross_function_entry_call_targets(
    ops: &mut [IlOp],
    maps: &[HashMap<u32, u32>],
    entry_labels: &HashMap<u32, u32>,
) {
    use std::collections::HashSet;

    let local: HashSet<u32> = ops
        .iter()
        .filter_map(|op| match op {
            IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) => Some(*id),
            _ => None,
        })
        .collect();
    for op in ops.iter_mut() {
        if let IlOp::Entry { target, .. } = op {
            if local.contains(&target.0) {
                continue;
            }
            if let Some(new_id) = resolve_cross_function_entry(target.0, maps, entry_labels)
                && new_id != target.0 && !local.contains(&new_id) {
                    target.0 = new_id;
                }
        }
    }
}

/// A jump into another function (the setup region's `JMP → main`) targets
/// that function's entry: prefer the recorded entry remap, since `prior` is a
/// first-wins merge of every body's private label space and an earlier body
/// may reuse the same old id for an internal label.
fn remap_cross_function_jump_targets(
    ops: &mut [IlOp],
    prior: &HashMap<u32, u32>,
    entry_labels: &HashMap<u32, u32>,
    flat_label_ids: &std::collections::HashSet<u32>,
) {
    use std::collections::HashSet;

    let local: HashSet<u32> = ops
        .iter()
        .filter_map(|op| match op {
            IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) => Some(*id),
            _ => None,
        })
        .collect();
    for op in ops.iter_mut() {
        if let IlOp::Jump { target, .. } = op {
            if local.contains(&target.0) {
                continue;
            }
            if flat_label_ids.contains(&target.0) {
                continue;
            }
            if let Some(&new_id) = entry_labels.get(&target.0) {
                target.0 = new_id;
                continue;
            }
            if let Some(&new_id) = prior.get(&target.0)
                && new_id != target.0 && !local.contains(&new_id) {
                    target.0 = new_id;
                }
        }
    }
}

/// The cursor the loop's back edge settles on: the entry cursor (the body
/// with this back edge cut), then one iteration from the header at a time
/// until the latch reproduces the assumed header cursor.
fn loop_back_edge_tell(
    ops: &[IlOp],
    lp: &super::analysis::NaturalLoop,
    entry_sp: u32,
) -> Option<u32> {
    let halt = || IlOp::Halt {
        loc: common::DebugLoc::unknown(),
    };
    let mut cut = ops.to_vec();
    cut[lp.latch] = halt();
    let mut at = super::tell::analyze_il_at(&cut, entry_sp)
        .tell_before(lp.header)
        .known()?;
    let entry = at;
    let mut once: Vec<IlOp> = ops[lp.header..lp.latch].to_vec();
    once.push(halt());
    for _ in 0..4 {
        let next = super::tell::analyze_il_at(&once, at).tell_before(once.len() - 1).known()?;
        if next == at {
            return (at > entry).then_some(at);
        }
        at = next;
    }
    None
}

/// An op that may allocate or call (a GC safepoint in an interpreted body).
fn il_may_collect(op: &IlOp) -> bool {
    use common::Instruction;
    if matches!(
        op,
        IlOp::Entry { .. }
            | IlOp::MakeTuple { .. }
            | IlOp::MakeArray { .. }
            | IlOp::MakeEnum { .. }
            | IlOp::BoxValue { .. }
            | IlOp::HostInvoke { .. }
            | IlOp::String { .. }
    ) {
        return true;
    }
    op.as_encode_byte().is_some_and(|b| {
        matches!(
            *b.bytecode(),
            Instruction::CALL
                | Instruction::CallIndirect
                | Instruction::InitTyped
                | Instruction::INIT
                | Instruction::ArrayPush
                | Instruction::MakeDict
                | Instruction::FORMAT
                | Instruction::STRINGIFY
                | Instruction::MakeFn
                | Instruction::MakePolyFnCapture
                | Instruction::DictEntries
                | Instruction::MakeEnumK
                | Instruction::MakeTupleK
                | Instruction::MakeCoro
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::il::op::{EntryKind, IlJumpKind, IlOp, Label};
    use common::{DebugLoc, Instruction};

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

    /// `JMP L` straight into `L` (past other labels) is a fall-through and
    /// goes; a jump over code stays (#771).
    #[test]
    fn drops_only_jumps_to_the_next_label() {
        let jmp = |l: u32| IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(l),
            loc: loc(),
            hint: Default::default(),
        };
        let mut ops = vec![
            IlOp::Load { slot: 0, loc: loc() },
            jmp(2),
            IlOp::Label(Label(1)),
            IlOp::Label(Label(2)),
            jmp(3),
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Label(Label(3)),
            IlOp::Return { loc: loc(), ret_words: 1 },
        ];
        drop_jumps_to_next_label(&mut ops);
        assert_eq!(ops.len(), 7);
        assert!(matches!(ops[1], IlOp::Label(Label(1))));
        assert!(matches!(ops[3], IlOp::Jump { target: Label(3), .. }));
    }

    #[test]
    fn from_flat_splits_prologue_body_epilogue() {
        let ops = vec![
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Pop { loc: loc() },
            IlOp::Const { imm: 2, loc: loc() },
            IlOp::Return { loc: loc(), ret_words: 1},
            IlOp::Halt { loc: loc() },
        ];
        let funcs = vec![IlFunc::new("f", None, 2, 4)];
        let m = IlModule::from_flat(&ops, &funcs);
        assert_eq!(m.prologue.len(), 2);
        assert_eq!(m.funcs.len(), 1);
        assert_eq!(m.funcs[0].ops.len(), 2);
        assert_eq!(m.epilogue.len(), 1);
        assert_eq!(m.to_flat().0.len(), ops.len());
    }

    #[test]
    fn from_flat_preserves_inter_func_glue() {
        let ops = vec![
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Return { loc: loc(), ret_words: 1},
            IlOp::Dup { loc: loc() },
            IlOp::Pop { loc: loc() },
            IlOp::Const { imm: 2, loc: loc() },
            IlOp::Return { loc: loc(), ret_words: 1},
            IlOp::Halt { loc: loc() },
        ];
        let funcs = vec![IlFunc::new("a", None, 1, 3), IlFunc::new("b", None, 5, 7)];
        let m = IlModule::from_flat(&ops, &funcs);
        assert_eq!(m.prologue.len(), 1);
        assert_eq!(m.funcs.len(), 2);
        assert_eq!(m.glue.len(), 1);
        assert_eq!(m.glue[0].len(), 2);
        assert!(matches!(m.glue[0][0], IlOp::Dup { .. }));
        assert_eq!(m.epilogue.len(), 1);
        assert_eq!(m.to_flat().0.len(), ops.len());
    }

    #[test]
    fn to_flat_gives_each_function_a_distinct_label_namespace() {
        let loc = loc();
        let mut m = IlModule::default();
        m.funcs.push(IlFuncBody {
            meta: IlFunc::new("a", None, 0, 3),
            ops: vec![
                IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    target: Label(0),
                    loc,
                    hint: Default::default(),
                },
                IlOp::Label(Label(0)),
                IlOp::Return { loc, ret_words: 1},
            ],
        });
        m.funcs.push(IlFuncBody {
            meta: IlFunc::new("b", None, 0, 3),
            ops: vec![
                IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    target: Label(0),
                    loc,
                    hint: Default::default(),
                },
                IlOp::Label(Label(0)),
                IlOp::Return { loc, ret_words: 1},
            ],
        });
        let (flat, _, _) = m.to_flat();
        let mut label_ids = Vec::new();
        for op in &flat {
            if let IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) = op {
                label_ids.push(*id);
            }
        }
        assert_eq!(
            label_ids.len(),
            label_ids
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            "flat IL must not reuse label ids across functions: {label_ids:?}"
        );
    }

    #[test]
    fn to_flat_remaps_cross_function_entry_call_targets() {
        let loc = loc();
        let callee_entry = Label(10);
        let mut m = IlModule::default();
        m.funcs.push(IlFuncBody {
            meta: IlFunc::new("callee", Some(callee_entry), 0, 2),
            ops: vec![IlOp::Label(callee_entry), IlOp::Return { loc, ret_words: 1}],
        });
        m.funcs.push(IlFuncBody {
            meta: IlFunc::new("caller", None, 0, 2),
            ops: vec![
                IlOp::Entry {
                    kind: EntryKind::Call,
                    arity: 1,
                    target: callee_entry,
                    loc, ret_words: 1,},
                IlOp::Return { loc, ret_words: 1},
            ],
        });
        let (flat, _, _) = m.to_flat();
        let callee_label = flat
            .iter()
            .find_map(|op| match op {
                IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) => Some(*id),
                _ => None,
            })
            .expect("callee entry label");
        let entry_target = flat
            .iter()
            .find_map(|op| match op {
                IlOp::Entry { target, .. } => Some(target.0),
                _ => None,
            })
            .expect("caller Entry");
        assert_eq!(
            entry_target, callee_label,
            "cross-function Entry must use the remapped callee entry label"
        );
    }

    /// A later callee's emit-time entry id can equal an earlier function's
    /// loop/preheader label. CALL must follow `IlFunc.entry`, not first-wins
    /// old→new (variadic `sum` + `greet`).
    #[test]
    fn to_flat_remaps_entry_when_callee_label_count_overlaps_old_target() {
        let loc = loc();
        let callee_entry = Label(2);
        let mut m = IlModule::default();
        m.funcs.push(IlFuncBody {
            meta: IlFunc::new("decoy", Some(Label(0)), 0, 4),
            ops: vec![
                IlOp::Label(Label(0)),
                IlOp::Label(Label(1)),
                IlOp::Label(callee_entry),
                IlOp::Return { loc, ret_words: 1},
            ],
        });
        m.funcs.push(IlFuncBody {
            meta: IlFunc::new("callee", Some(callee_entry), 0, 2),
            ops: vec![IlOp::Label(callee_entry), IlOp::Return { loc, ret_words: 1}],
        });
        m.funcs.push(IlFuncBody {
            meta: IlFunc::new("caller", None, 0, 2),
            ops: vec![
                IlOp::Entry {
                    kind: EntryKind::Call,
                    arity: 1,
                    target: callee_entry,
                    loc, ret_words: 1,},
                IlOp::Return { loc, ret_words: 1},
            ],
        });
        let (flat, _, _) = m.to_flat();
        let callee_label = flat
            .iter()
            .filter_map(|op| match op {
                IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) => Some(*id),
                _ => None,
            })
            .nth(3)
            .expect("callee entry is the fourth label (after decoy's three)");
        let entry_target = flat
            .iter()
            .find_map(|op| match op {
                IlOp::Entry { target, .. } => Some(target.0),
                _ => None,
            })
            .expect("caller Entry");
        assert_eq!(
            entry_target, callee_label,
            "CALL target {entry_target} must be the callee entry {callee_label}, not the decoy's reused id"
        );
    }

    #[test]
    fn to_flat_remaps_codeptr_when_callee_label_count_overlaps_old_target() {
        let loc = loc();
        let callee_entry = Label(2);
        let mut m = IlModule::default();
        m.funcs.push(IlFuncBody {
            meta: IlFunc::new("decoy", Some(Label(0)), 0, 4),
            ops: vec![
                IlOp::Label(Label(0)),
                IlOp::Label(Label(1)),
                IlOp::Label(callee_entry),
                IlOp::Return { loc, ret_words: 1},
            ],
        });
        m.funcs.push(IlFuncBody {
            meta: IlFunc::new("callee", Some(callee_entry), 0, 2),
            ops: vec![IlOp::Label(callee_entry), IlOp::Return { loc, ret_words: 1}],
        });
        m.funcs.push(IlFuncBody {
            meta: IlFunc::new("caller", None, 0, 2),
            ops: vec![
                IlOp::Entry {
                    kind: EntryKind::CodePtr,
                    arity: 0,
                    target: callee_entry,
                    loc, ret_words: 1,},
                IlOp::Return { loc, ret_words: 1},
            ],
        });
        let (flat, _, _) = m.to_flat();
        let callee_label = flat
            .iter()
            .filter_map(|op| match op {
                IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) => Some(*id),
                _ => None,
            })
            .nth(3)
            .expect("callee entry is the fourth label (after decoy's three)");
        let entry_target = flat
            .iter()
            .find_map(|op| match op {
                IlOp::Entry {
                    kind: EntryKind::CodePtr,
                    target,
                    ..
                } => Some(target.0),
                _ => None,
            })
            .expect("caller CodePtr");
        assert_eq!(
            entry_target, callee_label,
            "CodePtr target {entry_target} must be the callee entry {callee_label}, not the decoy's reused id"
        );
    }

    #[test]
    fn to_flat_remaps_call_when_entry_label_was_relabeled() {
        let loc = loc();
        let emit_entry = Label(10);
        let mut m = IlModule::default();
        m.funcs.push(IlFuncBody {
            meta: IlFunc::new("callee", Some(emit_entry), 0, 2),
            ops: vec![IlOp::Label(Label(3)), IlOp::Return { loc, ret_words: 1}],
        });
        m.funcs.push(IlFuncBody {
            meta: IlFunc::new("caller", None, 0, 2),
            ops: vec![
                IlOp::Entry {
                    kind: EntryKind::Call,
                    arity: 0,
                    target: emit_entry,
                    loc, ret_words: 1,},
                IlOp::Return { loc, ret_words: 1},
            ],
        });
        let (flat, _, _) = m.to_flat();
        let callee_label = flat
            .iter()
            .find_map(|op| match op {
                IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) => Some(*id),
                _ => None,
            })
            .expect("callee body label");
        let entry_target = flat
            .iter()
            .find_map(|op| match op {
                IlOp::Entry { target, .. } => Some(target.0),
                _ => None,
            })
            .expect("caller Entry");
        assert_eq!(
            entry_target, callee_label,
            "CALL to emit-time entry must follow the body's surviving label"
        );
    }

    /// A decoy loop label uniquely owns the callee's emit-time entry id after
    /// the callee body was relabeled. CALL must still follow `IlFunc.entry`.
    #[test]
    fn to_flat_prefers_recorded_entry_over_unique_internal_label() {
        let loc = loc();
        let emit_entry = Label(10);
        let mut m = IlModule::default();
        m.funcs.push(IlFuncBody {
            meta: IlFunc::new("decoy", Some(Label(0)), 0, 3),
            ops: vec![
                IlOp::Label(Label(0)),
                IlOp::Label(emit_entry),
                IlOp::Return { loc, ret_words: 1},
            ],
        });
        m.funcs.push(IlFuncBody {
            meta: IlFunc::new("callee", Some(emit_entry), 0, 2),
            ops: vec![IlOp::Label(Label(3)), IlOp::Return { loc, ret_words: 1}],
        });
        m.funcs.push(IlFuncBody {
            meta: IlFunc::new("caller", None, 0, 2),
            ops: vec![
                IlOp::Entry {
                    kind: EntryKind::Call,
                    arity: 0,
                    target: emit_entry,
                    loc, ret_words: 1,},
                IlOp::Return { loc, ret_words: 1},
            ],
        });
        let (flat, _, _) = m.to_flat();
        let callee_label = flat
            .iter()
            .filter_map(|op| match op {
                IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) => Some(*id),
                _ => None,
            })
            .nth(2)
            .expect("callee entry is the third label");
        let entry_target = flat
            .iter()
            .find_map(|op| match op {
                IlOp::Entry { target, .. } => Some(target.0),
                _ => None,
            })
            .expect("caller Entry");
        assert_eq!(
            entry_target, callee_label,
            "recorded entry must win over a unique decoy loop label"
        );
    }

    #[test]
    fn to_flat_remaps_call_into_a_glue_body_despite_a_reused_id() {
        // A dictionary adapter thunk has no recorded function: it sits in the
        // glue after `method`. Another body's opts minted the same id, so the
        // unique-map fallback is ambiguous; the glue binding must still win.
        let loc = loc();
        let thunk = Label(7);
        let mut m = IlModule::default();
        m.funcs.push(IlFuncBody {
            meta: IlFunc::new("method", Some(Label(1)), 0, 1),
            ops: vec![IlOp::Label(Label(1)), IlOp::Return { loc, ret_words: 1 }],
        });
        m.glue.push(vec![IlOp::Label(thunk), IlOp::Return { loc, ret_words: 1 }]);
        m.funcs.push(IlFuncBody {
            meta: IlFunc::new("other", Some(Label(2)), 0, 1),
            ops: vec![IlOp::Label(Label(2)), IlOp::Label(thunk), IlOp::Return { loc, ret_words: 1 }],
        });
        m.glue.push(Vec::new());
        m.funcs.push(IlFuncBody {
            meta: IlFunc::new("caller", Some(Label(3)), 0, 2),
            ops: vec![
                IlOp::Label(Label(3)),
                IlOp::Entry { kind: EntryKind::Call, arity: 0, target: thunk, loc, ret_words: 1 },
                IlOp::Return { loc, ret_words: 1 },
            ],
        });
        let (flat, _, _) = m.to_flat();
        let bound: Vec<u32> = flat
            .iter()
            .filter_map(|op| match op {
                IlOp::Label(Label(id)) => Some(*id),
                _ => None,
            })
            .collect();
        let target = flat
            .iter()
            .find_map(|op| match op {
                IlOp::Entry { target, .. } => Some(target.0),
                _ => None,
            })
            .expect("caller Entry");
        assert_eq!(target, bound[1], "the CALL lands on the glue thunk; labels {bound:?}");
    }

    /// Typeclass / default-method CALLs target a body label that is not
    /// `IlFunc.entry`; unique-hit still remaps those.
    #[test]
    fn to_flat_remaps_unique_non_entry_call_target() {
        let loc = loc();
        let method = Label(7);
        let mut m = IlModule::default();
        m.funcs.push(IlFuncBody {
            meta: IlFunc::new("method", Some(Label(1)), 0, 3),
            ops: vec![
                IlOp::Label(Label(1)),
                IlOp::Label(method),
                IlOp::Return { loc, ret_words: 1},
            ],
        });
        m.funcs.push(IlFuncBody {
            meta: IlFunc::new("caller", None, 0, 2),
            ops: vec![
                IlOp::Entry {
                    kind: EntryKind::Call,
                    arity: 0,
                    target: method,
                    loc, ret_words: 1,},
                IlOp::Return { loc, ret_words: 1},
            ],
        });
        let (flat, _, _) = m.to_flat();
        let method_label = flat
            .iter()
            .filter_map(|op| match op {
                IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) => Some(*id),
                _ => None,
            })
            .nth(1)
            .expect("method body label");
        let entry_target = flat
            .iter()
            .find_map(|op| match op {
                IlOp::Entry { target, .. } => Some(target.0),
                _ => None,
            })
            .expect("caller Entry");
        assert_eq!(
            entry_target, method_label,
            "unique non-entry CALL must follow the method body label"
        );
    }

    /// `if x != 10 { raise }` binds `end_label` after the last RETURN, so the
    /// label lives in epilogue. A prior dense body that reused emit id 8 must
    /// not steal that jump (S3b reverse-index / times_a checksum OOB).
    #[test]
    fn to_flat_keeps_trailing_if_end_label_on_last_func() {
        let loc = loc();
        let mut m = IlModule::default();
        m.funcs.push(IlFuncBody {
            meta: IlFunc::new("sum", Some(Label(3)), 0, 4),
            ops: vec![
                IlOp::Label(Label(3)),
                IlOp::Label(Label(8)),
                IlOp::Return { loc, ret_words: 1 },
            ],
        });
        m.funcs.push(IlFuncBody {
            meta: IlFunc::new("main", Some(Label(7)), 0, 3),
            ops: vec![
                IlOp::Label(Label(7)),
                IlOp::Jump {
                    kind: IlJumpKind::JumpIfFalse,
                    target: Label(8),
                    loc,
                    hint: Default::default(),
                },
                IlOp::Return { loc, ret_words: 1 },
            ],
        });
        m.epilogue = vec![IlOp::Label(Label(8))];
        let (flat, _, _) = m.to_flat();
        let main_jmp = flat
            .iter()
            .find_map(|op| match op {
                IlOp::Jump {
                    target,
                    kind: IlJumpKind::JumpIfFalse,
                    ..
                } => Some(target.0),
                _ => None,
            })
            .expect("main JMPF");
        let last_label = flat.iter().rev().find_map(|op| match op {
            IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) => Some(*id),
            _ => None,
        });
        assert_eq!(
            Some(main_jmp),
            last_label,
            "main skip-raise must bind the trailing end-label, not sum's reused id 8"
        );
        let sum_mid = flat
            .iter()
            .filter_map(|op| match op {
                IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) => Some(*id),
                _ => None,
            })
            .nth(1)
            .expect("sum's second label");
        assert_ne!(
            main_jmp, sum_mid,
            "main must not jump into sum's remapped Label(8)"
        );
    }

    /// Trailing if-end labels must stay with the jumper, not the next body's
    /// leading labels. MIR replace of the next body would otherwise drop them
    /// (COI-407 release `label was never bound`).
    #[test]
    fn from_flat_keeps_trailing_if_end_on_previous_func() {
        super::prove_trailing_if_end_after_next_body_replace();
    }

    #[test]
    fn to_flat_remaps_prologue_codeptr_entry_targets() {
        let loc = loc();
        let drop_entry = Label(0);
        let mut m = IlModule {
            prologue: vec![
                IlOp::Entry {
                    kind: EntryKind::CodePtr,
                    arity: 0,
                    target: drop_entry,
                    loc, ret_words: 1,},
                IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    target: Label(99),
                    loc,
                    hint: Default::default(),
                },
            ],
            ..Default::default()
        };
        m.funcs.push(IlFuncBody {
            meta: IlFunc::new("drop", Some(drop_entry), 0, 2),
            ops: vec![IlOp::Label(drop_entry), IlOp::Return { loc, ret_words: 1}],
        });
        let (flat, _, _) = m.to_flat();
        let drop_label = flat
            .iter()
            .find_map(|op| match op {
                IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) => Some(*id),
                _ => None,
            })
            .expect("drop entry label");
        let codeptr_target = flat
            .iter()
            .find_map(|op| match op {
                IlOp::Entry {
                    kind: EntryKind::CodePtr,
                    target,
                    .. } => Some(target.0),
                _ => None,
            })
            .expect("prologue CodePtr");
        assert_eq!(
            codeptr_target, drop_label,
            "prologue CodePtr must use the remapped drop entry label"
        );
    }

    #[test]
    fn with_entries_preserves_entry_map() {
        let ops = vec![
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        let funcs = vec![IlFunc::new("f", Some(Label(9)), 0, 2)];
        let mut entries = HashMap::new();
        entries.insert(0usize, Label(9));
        let m = IlModule::from_flat(&ops, &funcs).with_entries(entries);
        assert_eq!(m.entry_at_offset.get(&0), Some(&Label(9)));
        assert_eq!(m.funcs[0].meta.entry, Some(Label(9)));
    }

    /// Empty `funcs` must not discard a previously attached entry map when rebuilding.
    #[test]
    fn with_entries_survives_empty_funcs_from_flat() {
        let ops = vec![
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        let mut entries = HashMap::new();
        entries.insert(0usize, Label(3));
        let m = IlModule::from_flat(&ops, &[]).with_entries(entries);
        assert!(m.funcs.is_empty());
        assert_eq!(m.prologue.len(), 2);
        assert_eq!(m.entry_at_offset.get(&0), Some(&Label(3)));
    }

    #[test]
    fn empty_funcs_optimizes_whole_buffer() {
        let mut m = IlModule {
            prologue: vec![
                IlOp::Dup { loc: loc() },
                IlOp::Pop { loc: loc() },
                IlOp::Const { imm: 1, loc: loc() },
                IlOp::Return { loc: loc(), ret_words: 1},
            ],
            ..IlModule::default()
        };
        let (flat, _, _) = m.optimize_and_flatten(&OptimizeOptions::default(), &mut Vec::new());
        assert!(!flat.iter().any(|op| matches!(op, IlOp::Dup { .. })));
        assert!(flat.iter().any(
            |op| matches!(op, IlOp::ConstReturnImm { .. }) || matches!(op, IlOp::Return { .. })
        ));
    }

    #[test]
    fn optimize_and_flatten_dces_body_only() {
        let ops = vec![
            IlOp::Dup { loc: loc() },
            IlOp::Pop { loc: loc() },
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Dup { loc: loc() },
            IlOp::Pop { loc: loc() },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        let funcs = vec![IlFunc::new("f", None, 2, 6)];
        let mut m = IlModule::from_flat(&ops, &funcs);
        let (flat, _, _) = m.optimize_and_flatten(&OptimizeOptions::default(), &mut Vec::new());
        assert!(matches!(flat[0], IlOp::Dup { .. }));
        assert!(matches!(flat[1], IlOp::Pop { .. }));
        assert!(!flat[2..].iter().any(|op| matches!(op, IlOp::Dup { .. })));
        let _ = IlJumpKind::Unconditional;
        let _ = Label(0);
    }

    /// Raising loop used by Seek-normalize tests. Mandelbrot's innermost loop
    /// is not this shape (no tell-proven self-store); this IL is.
    fn raising_loop() -> Vec<IlOp> {
        vec![
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(0)),
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::StorePop {
                slot: 2,
                loc: loc(),
            },
            IlOp::Load {
                slot: 2,
                loc: loc(),
            },
            IlOp::Pop { loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
        ]
    }

    fn seek_promote_opts() -> OptimizeOptions {
        OptimizeOptions {
            dead_block: false,
            stack_dce: false,
            slot_promote: false,
            canon: false,
            algebraic: false,
            local_cse: false,
            licm: false,
            loop_bounds: false,
            clone_shared_return: false,
            loop_unroll: false,
            loop_unroll_factor: 8,
            escape_analysis: false,
            branch_optimization: false,
            collect_stats: false,
            mir_specialize: true,
        }
    }

    /// Production `optimize_and_flatten` must not Seek-normalize a raising
    /// loop (Seek-to-tell + drop store); that pass was removed.
    /// LIR reconstruct may rewrite the loop; that is not Seek-normalize.
    #[test]
    fn optimize_and_flatten_default_does_not_seek_normalize() {
        let ops = raising_loop();
        let emit_end = ops.iter().filter(|op| op.emits_code()).count();
        let funcs = vec![IlFunc::with_entry_sp("f", None, 0, emit_end, 2)];
        let mut m = IlModule::from_flat(&ops, &funcs);
        let (flat, _, _) = m.optimize_and_flatten(&seek_promote_opts(), &mut Vec::new());
        let seek_to = flat.iter().find_map(|op| match op {
            IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::Seek => {
                Some(byte.operand_u32())
            }
            _ => None,
        });
        let stores = flat
            .iter()
            .filter(|op| matches!(op, IlOp::StorePop { .. }))
            .count();
        assert!(
            !(seek_to == Some(2) && stores == 0),
            "default opts must not Seek-normalize the raising loop"
        );
        assert!(
            flat.iter().any(|op| matches!(
                op,
                IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    ..
                }
            )),
            "raising loop must keep a back-edge"
        );
    }

    /// A loop that allocates and stores a body local above the entry cursor
    /// gets a `CONST 0; STORE b-1` preheader so its header cursor is exact;
    /// a loop whose header cursor is already known is left alone.
    #[test]
    fn loop_cursor_raise_makes_the_header_cursor_exact() {
        let alloc = || IlOp::MakeArray {
            arity: 0,
            elem_kind: 0,
            loc: loc(),
        };
        let jump = |kind, target| IlOp::Jump {
            kind,
            target,
            loc: loc(),
            hint: Default::default(),
        };
        let ops = vec![
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::StorePop { slot: 1, loc: loc() },
            IlOp::Label(Label(1)),
            IlOp::Load { slot: 0, loc: loc() },
            jump(IlJumpKind::JumpIfFalse, Label(2)),
            alloc(),
            IlOp::StorePop { slot: 3, loc: loc() },
            jump(IlJumpKind::Unconditional, Label(1)),
            IlOp::Label(Label(2)),
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::Return { loc: loc(), ret_words: 1 },
        ];
        let emit_end = ops.iter().filter(|op| op.emits_code()).count();
        let funcs = vec![IlFunc::with_entry_sp("f", None, 0, emit_end, 1)];
        let mut m = IlModule::from_flat(&ops, &funcs);
        m.apply_loop_cursor_raises(&["fuse"]);
        let body = &m.funcs[0].ops;
        let header = body
            .iter()
            .position(|op| matches!(op, IlOp::Label(Label(1))))
            .expect("header label");
        assert!(
            matches!(body[header - 1], IlOp::StorePop { slot: 3, .. }),
            "raise to the back-edge cursor 4"
        );
        let tell = super::super::tell::analyze_il_at(body, 1);
        assert_eq!(tell.tell_before(header).known(), Some(4));

        // Already exact (or not interpreted): untouched.
        let mut m = IlModule::from_flat(&ops, &funcs);
        m.apply_loop_cursor_raises(&["dense"]);
        assert_eq!(m.funcs[0].ops.len(), ops.len());
    }

}
