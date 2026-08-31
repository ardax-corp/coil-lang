//! Per-function IL module: owning view rebuilt at finalize from flat emit.
//!
//! Cheap split of one [`super::CodeBuf`] — not a second IL language.
//! Codegen keeps a flat [`super::CodeBuf`] stream. At lower time the buffer is
//! split into owned function bodies (plus prologue / glue / epilogue), opts and
//! CFG GVN run per body, then the stream is concatenated for whole-buffer
//! `multi_op_join_convoy` and a single fuse/PC lower.

use std::collections::HashMap;

use super::func::IlFunc;
use super::op::{IlOp, Label};
use super::opt::{self, OptimizeOptions};

/// One function's owned IL ops (labels inclusive at span edges).
#[derive(Clone)]
pub struct IlFuncBody {
    /// Span / entry metadata from emit-time [`IlFunc`].
    #[allow(dead_code)] // retained for module hooks / diagnostics
    pub meta: IlFunc,
    pub ops: Vec<IlOp>,
}

/// Flat stream partitioned into prologue, function bodies, and glue.
///
/// Rebuilt at finalize; bodies are the source of truth for per-func opts/GVN
/// until [`Self::optimize_and_flatten`] concatenates for multi_op + lower.
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
    pub fn to_flat(&self) -> (Vec<IlOp>, HashMap<u32, u32>, Vec<HashMap<u32, u32>>) {
        let mut out = Vec::new();
        // New ids must not overlap old Label/Jump/Entry ids still sitting on
        // cross-function CALL sites until the post-concat patch.
        let mut next_label = self.max_code_label().saturating_add(1);
        let mut prior_labels = HashMap::new();
        let mut entry_labels = HashMap::new();
        let mut func_label_maps = Vec::new();
        let mut segment_ranges: Vec<(usize, usize)> = Vec::new();
        if !self.prologue.is_empty() {
            let start = out.len();
            out.extend(self.prologue.iter().cloned());
            segment_ranges.push((start, out.len()));
        }
        for (i, body) in self.funcs.iter().enumerate() {
            let start = out.len();
            let mut chunk = body.ops.clone();
            if let Some(g) = self.glue.get(i) {
                chunk.extend(g.iter().cloned());
            }
            let old_entry = body.meta.entry.map(|Label(id)| id);
            let (chunk, map) = opt::remap_label_space(&chunk, &mut next_label, &prior_labels);
            // Prefer the remapped emit-time entry. If opts dropped that id
            // (preheader / relabel), CALL still has to land on this body.
            let new_entry = old_entry
                .and_then(|old| map.get(&old).copied())
                .or_else(|| first_label_id(&chunk));
            if let (Some(old), Some(new)) = (old_entry, new_entry) {
                entry_labels.insert(old, new);
            }
            func_label_maps.push(map.clone());
            merge_remap_labels(&mut prior_labels, map);
            out.extend(chunk);
            segment_ranges.push((start, out.len()));
        }
        if !self.epilogue.is_empty() {
            let start = out.len();
            out.extend(self.epilogue.iter().cloned());
            segment_ranges.push((start, out.len()));
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
                    &flat_label_ids,
                );
            } else {
                remap_cross_function_jump_targets(
                    &mut out[start..end],
                    &prior_labels,
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

    /// Per-func opts (excluding multi_op) + CFG GVN on each body, then
    /// whole-buffer [`opt::multi_op_join_convoy`] on the concatenated stream.
    ///
    /// `pool` is the module const pool (`f64` / boxed int bits) for algebraic
    /// float identity / const-fold peeps (may push folded float results).
    pub fn optimize_and_flatten(
        &mut self,
        opts: &OptimizeOptions,
        pool: &mut Vec<u64>,
    ) -> (Vec<IlOp>, HashMap<u32, u32>, Vec<HashMap<u32, u32>>) {
        let mut per = opts.clone();
        let run_multi = per.multi_op_join_convoy;
        per.multi_op_join_convoy = false;
        // Guard inversion removes JMPs that whole-buffer multi_op matches on.
        let run_invert = per.invert_guard_branch;
        per.invert_guard_branch = false;
        // GVN reasons about slot defs; promotion removes the store that makes one
        // visible, so it runs after GVN has seen the body. Seek-normalize poisons
        // operand-height at the latch, so it also waits until after GVN.
        let run_slot_promote_tell = per.slot_promote_tell;
        let run_seek_back_edge = per.seek_back_edge;
        let run_ssa_gvn = per.ssa_gvn;
        per.slot_promote_tell = false;
        per.seek_back_edge = false;
        per.ssa_gvn = false;

        if self.funcs.is_empty() {
            let (mut ops, remap, func_maps) = self.to_flat();
            opt::optimize(&mut ops, opts, pool);
            return (ops, remap, func_maps);
        }

        let mut next_label = self
            .funcs
            .iter()
            .map(|b| opt::max_code_label(&b.ops))
            .chain(std::iter::once(opt::max_code_label(&self.prologue)))
            .chain(self.glue.iter().map(|g| opt::max_code_label(g)))
            .chain(std::iter::once(opt::max_code_label(&self.epilogue)))
            .max()
            .unwrap_or(0)
            .saturating_add(1);

        crate::profile::begin_pgo_module();
        let instrumenting = crate::profile::pgo_instrumenting();
        for body in &mut self.funcs {
            crate::profile::next_pgo_function(&body.meta.name);
            opt::optimize_at_with_labels(
                &mut body.ops,
                &per,
                body.meta.entry_sp as i32,
                pool,
                &mut next_label,
            );
            if instrumenting {
                // Counters sit on cleanup mid-IR; skip CFG-rewriting decision opts.
                crate::profile::instrument_for_pgo_named_mut(&mut body.ops, &body.meta.name);
                continue;
            }
            super::gvn::cfg_gvn_with(&mut body.ops, false);
            if run_seek_back_edge {
                opt::seek_normalize_back_edges(&mut body.ops, body.meta.entry_sp);
            }
            if run_slot_promote_tell {
                opt::slot_promote_at(&mut body.ops, body.meta.entry_sp);
            }
            if run_ssa_gvn {
                super::gvn::ssa_gvn(&mut body.ops);
            }
        }

        let (mut flat, remap, func_maps) = self.to_flat();
        if run_multi {
            opt::multi_op_join_convoy(&mut flat);
        }
        if run_invert {
            opt::invert_guard_branch(&mut flat);
        }
        (flat, remap, func_maps)
    }
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
            if let Some(new_id) = resolve_cross_function_entry(target.0, maps, entry_labels) {
                if new_id != target.0 && !local.contains(&new_id) {
                    target.0 = new_id;
                }
            }
        }
    }
}

fn remap_cross_function_jump_targets(
    ops: &mut [IlOp],
    prior: &HashMap<u32, u32>,
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
            if let Some(&new_id) = prior.get(&target.0) {
                if new_id != target.0 && !local.contains(&new_id) {
                    target.0 = new_id;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::il::op::{EntryKind, IlJumpKind, IlOp, Label};
    use common::{Byte, DebugLoc, Instruction};

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

    fn load_const_add_suffix() -> Vec<IlOp> {
        vec![
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: loc(),
            },
        ]
    }

    #[test]
    fn from_flat_splits_prologue_body_epilogue() {
        let ops = vec![
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Pop { loc: loc() },
            IlOp::Const { imm: 2, loc: loc() },
            IlOp::Return { loc: loc() },
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
            IlOp::Return { loc: loc() },
            IlOp::Dup { loc: loc() },
            IlOp::Pop { loc: loc() },
            IlOp::Const { imm: 2, loc: loc() },
            IlOp::Return { loc: loc() },
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
                IlOp::Return { loc },
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
                IlOp::Return { loc },
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
            ops: vec![IlOp::Label(callee_entry), IlOp::Return { loc }],
        });
        m.funcs.push(IlFuncBody {
            meta: IlFunc::new("caller", None, 0, 2),
            ops: vec![
                IlOp::Entry {
                    kind: EntryKind::Call,
                    arity: 1,
                    target: callee_entry,
                    loc,
                },
                IlOp::Return { loc },
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
                IlOp::Return { loc },
            ],
        });
        m.funcs.push(IlFuncBody {
            meta: IlFunc::new("callee", Some(callee_entry), 0, 2),
            ops: vec![IlOp::Label(callee_entry), IlOp::Return { loc }],
        });
        m.funcs.push(IlFuncBody {
            meta: IlFunc::new("caller", None, 0, 2),
            ops: vec![
                IlOp::Entry {
                    kind: EntryKind::Call,
                    arity: 1,
                    target: callee_entry,
                    loc,
                },
                IlOp::Return { loc },
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
    fn to_flat_remaps_call_when_entry_label_was_relabeled() {
        let loc = loc();
        let emit_entry = Label(10);
        let mut m = IlModule::default();
        m.funcs.push(IlFuncBody {
            meta: IlFunc::new("callee", Some(emit_entry), 0, 2),
            ops: vec![IlOp::Label(Label(3)), IlOp::Return { loc }],
        });
        m.funcs.push(IlFuncBody {
            meta: IlFunc::new("caller", None, 0, 2),
            ops: vec![
                IlOp::Entry {
                    kind: EntryKind::Call,
                    arity: 0,
                    target: emit_entry,
                    loc,
                },
                IlOp::Return { loc },
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
                IlOp::Return { loc },
            ],
        });
        m.funcs.push(IlFuncBody {
            meta: IlFunc::new("callee", Some(emit_entry), 0, 2),
            ops: vec![IlOp::Label(Label(3)), IlOp::Return { loc }],
        });
        m.funcs.push(IlFuncBody {
            meta: IlFunc::new("caller", None, 0, 2),
            ops: vec![
                IlOp::Entry {
                    kind: EntryKind::Call,
                    arity: 0,
                    target: emit_entry,
                    loc,
                },
                IlOp::Return { loc },
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
                IlOp::Return { loc },
            ],
        });
        m.funcs.push(IlFuncBody {
            meta: IlFunc::new("caller", None, 0, 2),
            ops: vec![
                IlOp::Entry {
                    kind: EntryKind::Call,
                    arity: 0,
                    target: method,
                    loc,
                },
                IlOp::Return { loc },
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

    #[test]
    fn to_flat_remaps_prologue_codeptr_entry_targets() {
        let loc = loc();
        let drop_entry = Label(0);
        let mut m = IlModule::default();
        m.prologue = vec![
            IlOp::Entry {
                kind: EntryKind::CodePtr,
                arity: 0,
                target: drop_entry,
                loc,
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(99),
                loc,
                hint: Default::default(),
            },
        ];
        m.funcs.push(IlFuncBody {
            meta: IlFunc::new("drop", Some(drop_entry), 0, 2),
            ops: vec![IlOp::Label(drop_entry), IlOp::Return { loc }],
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
                    ..
                } => Some(target.0),
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
            IlOp::Return { loc: loc() },
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
            IlOp::Return { loc: loc() },
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
                IlOp::Return { loc: loc() },
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
            IlOp::Return { loc: loc() },
        ];
        let funcs = vec![IlFunc::new("f", None, 2, 6)];
        let mut m = IlModule::from_flat(&ops, &funcs);
        let (flat, _, _) = m.optimize_and_flatten(
            &OptimizeOptions {
                multi_op_join_convoy: false,
                ..OptimizeOptions::default()
            },
            &mut Vec::new(),
        );
        assert!(matches!(flat[0], IlOp::Dup { .. }));
        assert!(matches!(flat[1], IlOp::Pop { .. }));
        assert!(!flat[2..].iter().any(|op| matches!(op, IlOp::Dup { .. })));
        let _ = IlJumpKind::Unconditional;
        let _ = Label(0);
    }

    #[test]
    fn multi_op_on_full_buffer_refuses_when_prologue_poisons_sp() {
        let suf = load_const_add_suffix();
        let cond = IlOp::Const { imm: 1, loc: loc() };
        let mut ops = vec![IlOp::byte(Byte::new(Instruction::PRINT))];
        let body_start = ops.len();
        ops.extend(suf.clone());
        ops.push(cond.clone());
        ops.push(IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            target: Label(0),
            loc: loc(),
            hint: Default::default(),
        });
        ops.push(IlOp::Pop { loc: loc() });
        ops.extend(suf);
        ops.push(cond);
        ops.push(IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            target: Label(0),
            loc: loc(),
            hint: Default::default(),
        });
        ops.push(IlOp::Label(Label(0)));
        ops.push(IlOp::Return { loc: loc() });
        let body_emit_end = ops.iter().filter(|op| op.emits_code()).count();

        let mut body_only: Vec<IlOp> = ops[body_start..].to_vec();
        opt::multi_op_join_convoy(&mut body_only);
        let scoped_loads = body_only
            .iter()
            .filter(|op| matches!(op, IlOp::Load { .. }))
            .count();
        assert_eq!(
            scoped_loads, 1,
            "precondition: body-only multi_op sinks (Known SP)"
        );

        let funcs = vec![IlFunc::new("f", None, 1, body_emit_end)];
        let mut m = IlModule::from_flat(&ops, &funcs);
        let (flat, _, _) = m.optimize_and_flatten(&OptimizeOptions::default(), &mut Vec::new());
        let loads = flat
            .iter()
            .filter(|op| matches!(op, IlOp::Load { .. }))
            .count();
        assert_eq!(
            loads, 2,
            "full-buffer multi_op must refuse when prologue poisons SP"
        );
    }

    #[test]
    fn multi_op_on_full_buffer_still_sinks_clean_body() {
        let suf = load_const_add_suffix();
        let cond = IlOp::Const { imm: 1, loc: loc() };
        let mut ops = Vec::new();
        ops.extend(suf.clone());
        ops.push(cond.clone());
        ops.push(IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            target: Label(0),
            loc: loc(),
            hint: Default::default(),
        });
        ops.push(IlOp::Pop { loc: loc() });
        ops.extend(suf);
        ops.push(cond);
        ops.push(IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            target: Label(0),
            loc: loc(),
            hint: Default::default(),
        });
        ops.push(IlOp::Label(Label(0)));
        ops.push(IlOp::Return { loc: loc() });
        let emit_end = ops.iter().filter(|op| op.emits_code()).count();
        let funcs = vec![IlFunc::new("f", None, 0, emit_end)];
        let mut m = IlModule::from_flat(&ops, &funcs);
        let (flat, _, _) = m.optimize_and_flatten(
            &OptimizeOptions {
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
                multi_op_join_convoy: true,
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
                pure_call_ctx: None,
            },
            &mut Vec::new(),
        );
        let loads = flat
            .iter()
            .filter(|op| matches!(op, IlOp::Load { .. }))
            .count();
        assert_eq!(
            loads, 1,
            "clean body must still sink via whole-buffer multi_op"
        );
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

    fn seek_promote_opts(on: bool) -> OptimizeOptions {
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
            slot_promote_tell: true,
            seek_back_edge: on,
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
            pure_call_ctx: None,
        }
    }

    fn is_seek(op: &IlOp) -> bool {
        matches!(op, IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::Seek)
    }

    /// Production `optimize_and_flatten` (GVN, then Seek, then promote). Default
    /// flag is off, so Unknown headers keep the self-store.
    #[test]
    fn optimize_and_flatten_default_does_not_seek_normalize() {
        let ops = raising_loop();
        let emit_end = ops.iter().filter(|op| op.emits_code()).count();
        let funcs = vec![IlFunc::with_entry_sp("f", None, 0, emit_end, 2)];
        let mut m = IlModule::from_flat(&ops, &funcs);
        let (flat, _, _) = m.optimize_and_flatten(&seek_promote_opts(false), &mut Vec::new());
        assert!(!flat.iter().any(is_seek));
        let stores = flat
            .iter()
            .filter(|op| matches!(op, IlOp::StorePop { .. }))
            .count();
        assert_eq!(stores, 1);
    }

    /// Flag on: Seek sits on the latch after GVN, then promotion drops the
    /// self-store. Mandelbrot never takes this path (`seek_back_edge` stays off).
    #[test]
    fn optimize_and_flatten_seek_back_edge_elides_raising_loop_store() {
        let ops = raising_loop();
        let emit_end = ops.iter().filter(|op| op.emits_code()).count();
        let funcs = vec![IlFunc::with_entry_sp("f", None, 0, emit_end, 2)];
        let mut m = IlModule::from_flat(&ops, &funcs);
        let (flat, _, _) = m.optimize_and_flatten(&seek_promote_opts(true), &mut Vec::new());
        assert!(
            flat.windows(2).any(|w| {
                is_seek(&w[0])
                    && matches!(
                        w[1],
                        IlOp::Jump {
                            kind: IlJumpKind::Unconditional,
                            ..
                        }
                    )
            }),
            "Seek must sit on the latch after GVN"
        );
        let seek_to = flat.iter().find_map(|op| match op {
            IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::Seek => {
                Some(byte.operand_u32())
            }
            _ => None,
        });
        assert_eq!(
            seek_to,
            Some(2),
            "Seek must re-anchor to the forward-edge tell"
        );
        let stores = flat
            .iter()
            .filter(|op| matches!(op, IlOp::StorePop { .. }))
            .count();
        assert_eq!(stores, 0);
    }
}
