//! Per-function IL module: owning view rebuilt at finalize from flat emit.
//!
//! Cheap split of one [`super::CodeBuf`] — not a second IL language.
//! Codegen keeps a flat [`super::CodeBuf`] stream. At lower time the buffer is
//! split into owned function bodies (plus prologue / glue / epilogue), each
//! body's MIR emits dense or LIR in place of its placeholder, then the
//! stream is concatenated for a single fuse/PC lower.

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
    /// I7 / C3 deopt resume drafts (compiler-internal; not archived).
    pub deopt_map_drafts: Vec<crate::mir::DraftDeoptMap>,
    /// Allocating dense bodies, whose frames need a decoded extent.
    pub needs_frame_extent: HashSet<String>,
    /// Original IL slot → reconstruct slot after dense / LIR (named lets).
    pub debug_slot_remaps: HashMap<String, HashMap<u32, u32>>,
    /// A pinned (`defer`) body the LIR tier took: its cleanup pad's label
    /// and frame words, in place of HIR emission's pad and thunks.
    pub lir_cleanup_pads: HashMap<String, (Label, u32)>,
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
                deopt_map_drafts: Vec::new(),
                needs_frame_extent: HashSet::new(),
                debug_slot_remaps: HashMap::new(),
                lir_cleanup_pads: HashMap::new(),
            };
        }

        let mut ranges: Vec<(usize, usize, usize)> = funcs
            .iter()
            .enumerate()
            .filter(|(_, f)| f.code_start < f.code_end)
            .map(|(i, f)| {
                let (s, e) = opt::emitting_range_to_raw(ops, f.code_start, f.code_end);
                (i, entry_label_start(ops, s, f.entry), e)
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
        // Old label ids bound in each body and in its glue (before remap),
        // and each glue's own remap (it differs from the body's on an id
        // both bind).
        let mut body_bound: Vec<HashSet<u32>> = Vec::new();
        let mut glue_bound: Vec<HashSet<u32>> = Vec::new();
        let mut glue_label_maps: Vec<HashMap<u32, u32>> = Vec::new();
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
            let body_len = body.ops.len();
            body_bound.push(bound_label_ids(&chunk[..body_len]));
            glue_bound.push(bound_label_ids(&chunk[body_len..]));
            let (mut chunk, map) = opt::remap_label_space(&chunk, &mut next_label, &prior_labels);
            // Per-body opts mint ids after emission, so the body may bind an
            // id its glue (default impls, adapter thunks) binds too. One map
            // would merge the two labels and send the body's branches into
            // the glue: give the glue's copy its own id.
            let mut glue_map = map.clone();
            for old in body_bound[i].intersection(&glue_bound[i]) {
                let shared = map[old];
                let fresh = next_label;
                next_label = next_label.saturating_add(1);
                glue_map.insert(*old, fresh);
                for op in &mut chunk[body_len..] {
                    match op {
                        IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) if *id == shared => *id = fresh,
                        IlOp::Jump { target, .. } | IlOp::Entry { target, .. } if target.0 == shared => {
                            target.0 = fresh;
                        }
                        _ => {}
                    }
                }
            }
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
                        && let Some(&new) = glue_map.get(old)
                    {
                        entry_labels.entry(*old).or_insert(new);
                    }
                }
            }
            func_label_maps.push(map.clone());
            merge_remap_labels(&mut prior_labels, map);
            glue_label_maps.push(glue_map);
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
            let owner = [k, k.wrapping_sub(1)]
                .into_iter()
                .filter(|&i| i < module.funcs.len())
                .find_map(|i| {
                    if body_bound[i].contains(&old) {
                        Some(&func_label_maps[i])
                    } else if glue_bound[i].contains(&old) {
                        Some(&glue_label_maps[i])
                    } else {
                        None
                    }
                });
            if let Some(new) = owner.and_then(|m| m.get(&old).copied()) {
                jump_entries.insert(old, new);
            }
        }
        // Prologue and epilogue are copied verbatim, so their labels keep
        // their ids; a body's opts may have minted the same id, and the
        // first-wins body map must not claim it (the prologue `JMP` to the
        // static-init setup would land in that body).
        for op in self.prologue.iter().chain(module.epilogue.iter()) {
            if let IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) = op {
                prior_labels.insert(*id, *id);
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

    /// MIR specialization of each body, then concatenate the bodies.
    ///
    /// `pool` is the module const pool, which MIR specialization reads and
    /// extends.
    pub fn optimize_and_flatten(
        &mut self,
        opts: &OptimizeOptions,
        pool: &mut Vec<u64>,
    ) -> FlatIl {
        if self.funcs.is_empty() {
            return self.to_flat();
        }

        for body in self.funcs.iter_mut().filter(|b| !b.meta.pinned) {
            drop_jumps_to_next_label(&mut body.ops);
        }

        // Leaf-first: a caller may take dense once every callee it CALLs is dense.
        // Every other body takes LIR from its MIR (lowered from HIR).
        self.needs_frame_extent.clear();
        let mut dense_calls = crate::layout::DenseCallMap::new();
        // Pinned (`defer`) bodies try LIR below, with their cleanup pad.
        let mut pending: Vec<usize> = (0..self.funcs.len())
            .filter(|&i| !self.funcs[i].meta.pinned)
            .collect();
        let mut dense_why: Vec<Option<String>> = vec![None; self.funcs.len()];
        let mut lir_why: Vec<Option<String>> = vec![None; self.funcs.len()];
        let mut tier: Vec<&'static str> = vec!["fuse"; self.funcs.len()];
        let mut side_remaps = HashMap::<String, HashMap<u32, u32>>::new();
        let mut side_deopts = Vec::new();
        // Every body takes its stack IL from its MIR, so its tier gates
        // read that MIR and the LIR emitted from it (N1b): of
        // emit_hir's stack IL only the labels it binds.
        let labels: Vec<crate::mir::BodyLabels> =
            self.funcs.iter().map(|b| crate::mir::BodyLabels::of(&b.ops)).collect();
        // Per body, once: the LIR its MIR emits (what runs if dense
        // refuses), `None` when it has no MIR. The LIR tier below keeps it.
        let mut fuse_fallbacks: Vec<Option<Option<crate::mir::LirDraft>>> =
            std::iter::repeat_with(|| None).take(self.funcs.len()).collect();
        while !pending.is_empty() {
            let mut next = Vec::new();
            let mut progressed = false;
            for i in pending.iter().copied() {
                let body = &mut self.funcs[i];
                let mut side = crate::mir::BodySidecar::default();
                let fallback = fuse_fallbacks[i].get_or_insert_with(|| {
                    let func = body.meta.hir_mir.as_deref()?;
                    Some(crate::mir::fuse_fallback(&labels[i], &body.meta.name, pool, func, opts))
                });
                if let Some((dense, abi)) = crate::mir::try_specialize_body_side(
                    fallback.as_ref().and_then(crate::mir::LirDraft::ops),
                    &labels[i],
                    &body.meta.name,
                    pool,
                    &dense_calls,
                    body.meta.entry,
                    body.meta.hir_mir.as_deref(),
                    &mut side,
                    opts,
                ) {
                    // Even empty: a local it leaves out has no register.
                    side_remaps.insert(body.meta.name.clone(), side.debug_slot_remap);
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
        for body in self.funcs.iter().filter(|b| b.meta.pinned) {
            if let Some(func) = body.meta.hir_mir.as_deref() {
                crate::mir::capture_pinned_mir(&body.meta.name, func);
            }
        }
        let dense_kept = self.funcs.len() - pending.len() - pinned;
        let mut lir_kept = 0usize;
        // A pinned (`defer`) body whose MIR has its cleanup pad may take
        // LIR, which emits that pad after the body; it stays pinned.
        let padded: Vec<usize> = (0..self.funcs.len())
            .filter(|&i| self.funcs[i].meta.pinned && self.funcs[i].meta.hir_mir.as_deref().is_some_and(|f| f.cleanup.is_some()))
            .collect();
        self.lir_cleanup_pads.clear();
        for i in pending.iter().copied().chain(padded.iter().copied()) {
            let body = &mut self.funcs[i];
            let mut side = crate::mir::BodySidecar::default();
            // A pending body's LIR was lowered once for the dense gates.
            if let Some(lir) = crate::mir::lower_abi_body_reusing(
                fuse_fallbacks[i].take().flatten(),
                &labels[i],
                &body.meta.name,
                pool,
                body.meta.hir_mir.as_deref(),
                &mut side,
                opts,
            ) {
                // Do not re-run stack-IL opts: `local_cse` refuses MOD and
                // rematerializes a stored remainder (pair_int_churn +12%).
                if body.meta.pinned && side.cleanup_pad.is_none() {
                    lir_why[i] = Some("no cleanup pad".to_string());
                } else {
                    if let Some(pad) = side.cleanup_pad.filter(|_| body.meta.pinned) {
                        self.lir_cleanup_pads.insert(body.meta.name.clone(), pad);
                    }
                    // Even empty: a local it leaves out has no register.
                    side_remaps.insert(body.meta.name.clone(), side.debug_slot_remap);
                    if let Some(deopt) = side.deopt {
                        side_deopts.push(deopt);
                    }
                    body.ops = lir;
                    tier[i] = "lir";
                    lir_kept += 1;
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
        // M5p: every body runs IL emitted from its MIR (dense or LIR).
        // One left on emit_hir's stack lowering is a compiler bug.
        let fell_back: Vec<String> = (0..self.funcs.len())
            // With `require_mir` (a clean compile) every body must have
            // MIR. Without it (an IL-only test module, or one past a
            // program error that never runs) only bodies HIR lowered count.
            .filter(|&i| {
                let meta = &self.funcs[i].meta;
                tier[i] == "fuse"
                    && (opts.require_mir || meta.hir_mir.is_some() || meta.hir_mir_why.is_some())
            })
            .map(|i| {
                let body = &self.funcs[i];
                let why = match &body.meta.hir_mir {
                    None => body.meta.hir_mir_why.clone().map_or("no MIR from HIR".to_string(), |w| format!("no MIR from HIR: {w}")),
                    Some(_) => lir_why[i].clone().or_else(|| dense_why[i].clone()).unwrap_or_else(|| "no tier took it".to_string()),
                };
                format!("`{}` ({why})", body.meta.name)
            })
            .collect();
        if !fell_back.is_empty() {
            panic!(
                "internal compiler error: {} bod{} fell back to the HIR stack lowering: {}",
                fell_back.len(),
                if fell_back.len() == 1 { "y" } else { "ies" },
                fell_back.join(", ")
            );
        }

        if opts.collect_stats {
            // A pinned body LIR took counts in `lir_kept`, not `pinned`.
            super::opt::note_body_tiers(dense_kept, lir_kept, pending.len() + pinned - lir_kept);
            for (i, body) in self.funcs.iter().enumerate() {
                super::opt::note_body_tier(super::opt::BodyTier {
                    name: body.meta.name.clone(),
                    tier: tier[i].to_string(),
                    dense_reason: dense_why[i].take(),
                    lir_reason: lir_why[i].take(),
                    hir_reason: match &body.meta.hir_mir {
                        Some(_) => Some("lowered".to_string()),
                        None => body.meta.hir_mir_why.clone(),
                    },
                });
            }
        }
        self.debug_slot_remaps.extend(side_remaps);
        self.deopt_map_drafts.extend(side_deopts);
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

/// Drop `JMP L` when `L` is the next label: it is a fall-through. Passes that
/// turn stack words into slots misread the words such a jump carries (an
/// `Unpack` payload went to the wrong slots, #771).
/// Move a body's raw start back over the labels before it to its own entry
/// label. [`opt::emitting_range_to_raw`] stops at a label some earlier op
/// jumps to, which is right for the previous body's trailing if-end but not
/// for `main`'s entry, which the setup prologue jumps to. Left in the gap,
/// that label shares the previous body's chunk, whose per-body opts may mint
/// the same id for a loop label; the two then flatten to one, and the loop's
/// back edge lands in `main` (#907).
fn entry_label_start(ops: &[IlOp], start: usize, entry: Option<Label>) -> usize {
    let Some(entry) = entry else {
        return start;
    };
    let mut s = start;
    while s > 0 && !ops[s - 1].emits_code() {
        match ops[s - 1].bind_label() {
            Some(l) if l == entry => return s - 1,
            Some(_) => s -= 1,
            None => return start,
        }
    }
    start
}

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

fn bound_label_ids(ops: &[IlOp]) -> HashSet<u32> {
    ops.iter()
        .filter_map(|op| match op {
            IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) => Some(*id),
            _ => None,
        })
        .collect()
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

    /// Per-body opts mint label ids after emission, so a body can bind an id
    /// its trailing glue (default `Show` / `String` impls) also binds. The
    /// body's branch must still land in the body.
    #[test]
    fn to_flat_keeps_body_jumps_off_glue_labels_with_the_same_id() {
        let loc = loc();
        let mut m = IlModule::default();
        m.funcs.push(IlFuncBody {
            meta: IlFunc::new("f", Some(Label(1)), 0, 4),
            ops: vec![
                IlOp::Label(Label(1)),
                IlOp::Jump {
                    kind: IlJumpKind::JumpIfFalse,
                    target: Label(3),
                    loc,
                    hint: Default::default(),
                },
                IlOp::Return { loc, ret_words: 1 },
                IlOp::Label(Label(3)),
                IlOp::Return { loc, ret_words: 1 },
            ],
        });
        m.glue.push(vec![IlOp::Label(Label(3)), IlOp::Return { loc, ret_words: 1 }]);
        let (flat, _, _) = m.to_flat();
        let labels: Vec<(usize, u32)> = flat
            .iter()
            .enumerate()
            .filter_map(|(i, op)| match op {
                IlOp::Label(Label(id)) => Some((i, *id)),
                _ => None,
            })
            .collect();
        let ids: HashSet<u32> = labels.iter().map(|(_, id)| *id).collect();
        assert_eq!(ids.len(), labels.len(), "flat IL binds a label twice: {labels:?}");
        let target = flat
            .iter()
            .find_map(|op| match op {
                IlOp::Jump { target, .. } => Some(target.0),
                _ => None,
            })
            .unwrap();
        let (at, _) = labels.iter().find(|(_, id)| *id == target).unwrap();
        assert_eq!(*at, 3, "the body's branch left the body: {labels:?} -> {target}");
    }

    /// The prologue (static-init setup region) is copied verbatim, so its
    /// labels keep their ids. A body's opts may mint the same id; the global
    /// remap must still resolve the prologue's label to itself, or the
    /// prologue `JMP` to the setup region lands in that body (#960).
    #[test]
    fn to_flat_keeps_prologue_labels_when_a_body_binds_the_same_id() {
        let loc = loc();
        let mut m = IlModule {
            prologue: vec![
                IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    target: Label(5),
                    loc,
                    hint: Default::default(),
                },
                IlOp::Halt { loc },
                IlOp::Label(Label(5)),
                IlOp::Halt { loc },
            ],
            ..Default::default()
        };
        m.funcs.push(IlFuncBody {
            meta: IlFunc::new("f", Some(Label(1)), 0, 4),
            ops: vec![
                IlOp::Label(Label(1)),
                IlOp::Jump {
                    kind: IlJumpKind::JumpIfFalse,
                    target: Label(5),
                    loc,
                    hint: Default::default(),
                },
                IlOp::Return { loc, ret_words: 1 },
                IlOp::Label(Label(5)),
                IlOp::Return { loc, ret_words: 1 },
            ],
        });
        let (flat, remap, _) = m.to_flat();
        assert_eq!(remap.get(&5).copied().unwrap_or(5), 5, "prologue label 5 remapped into the body: {remap:?}");
        assert!(
            matches!(flat[0], IlOp::Jump { target: Label(5), .. }),
            "prologue jump must keep its own label"
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
            require_mir: false,
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


}
