//! Per-function IL module: owning view rebuilt at finalize from flat emit.
//!
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
    pub fn to_flat(&self) -> Vec<IlOp> {
        let mut out = Vec::new();
        out.extend(self.prologue.iter().cloned());
        for (i, body) in self.funcs.iter().enumerate() {
            out.extend(body.ops.iter().cloned());
            if let Some(g) = self.glue.get(i) {
                out.extend(g.iter().cloned());
            }
        }
        out.extend(self.epilogue.iter().cloned());
        out
    }

    /// Per-func opts (excluding multi_op) + CFG GVN on each body, then
    /// whole-buffer [`opt::multi_op_join_convoy`] on the concatenated stream.
    ///
    /// `pool` is the module const pool (`f64` / boxed int bits) for algebraic
    /// float identity / const-fold peeps (may push folded float results).
    pub fn optimize_and_flatten(&mut self, opts: &OptimizeOptions, pool: &mut Vec<u64>) -> Vec<IlOp> {
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
        per.slot_promote_tell = false;
        per.seek_back_edge = false;

        if self.funcs.is_empty() {
            let mut ops = self.to_flat();
            opt::optimize(&mut ops, opts, pool);
            return ops;
        }

        for body in &mut self.funcs {
            opt::optimize_at(&mut body.ops, &per, body.meta.entry_sp as i32, pool);
            super::gvn::cfg_gvn_with(&mut body.ops, per.ssa_gvn);
            if run_seek_back_edge {
                opt::seek_normalize_back_edges(&mut body.ops, body.meta.entry_sp);
            }
            if run_slot_promote_tell {
                opt::slot_promote_at(&mut body.ops, body.meta.entry_sp);
            }
        }

        let mut flat = self.to_flat();
        if run_multi {
            opt::multi_op_join_convoy(&mut flat);
        }
        if run_invert {
            opt::invert_guard_branch(&mut flat);
        }
        flat
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::il::op::{IlJumpKind, IlOp, Label};
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
        assert_eq!(m.to_flat().len(), ops.len());
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
        assert_eq!(m.to_flat().len(), ops.len());
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
        let flat = m.optimize_and_flatten(&OptimizeOptions::default(), &mut Vec::new());
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
        let flat = m.optimize_and_flatten(
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
        });
        ops.push(IlOp::Pop { loc: loc() });
        ops.extend(suf);
        ops.push(cond);
        ops.push(IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            target: Label(0),
            loc: loc(),
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
        let flat = m.optimize_and_flatten(&OptimizeOptions::default(), &mut Vec::new());
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
        });
        ops.push(IlOp::Pop { loc: loc() });
        ops.extend(suf);
        ops.push(cond);
        ops.push(IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            target: Label(0),
            loc: loc(),
        });
        ops.push(IlOp::Label(Label(0)));
        ops.push(IlOp::Return { loc: loc() });
        let emit_end = ops.iter().filter(|op| op.emits_code()).count();
        let funcs = vec![IlFunc::new("f", None, 0, emit_end)];
        let mut m = IlModule::from_flat(&ops, &funcs);
        let flat = m.optimize_and_flatten(
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
                invariant_store_elim: false,
                ssa_gvn: false,
                escape_analysis: false,
                branch_optimization: false,
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
            },
            IlOp::Label(Label(0)),
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::StorePop { slot: 2, loc: loc() },
            IlOp::Load { slot: 2, loc: loc() },
            IlOp::Pop { loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
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
            invariant_store_elim: false,
            ssa_gvn: false,
            escape_analysis: false,
                branch_optimization: false,
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
        let flat = m.optimize_and_flatten(&seek_promote_opts(false), &mut Vec::new());
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
        let flat = m.optimize_and_flatten(&seek_promote_opts(true), &mut Vec::new());
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
        assert_eq!(seek_to, Some(2), "Seek must re-anchor to the forward-edge tell");
        let stores = flat
            .iter()
            .filter(|op| matches!(op, IlOp::StorePop { .. }))
            .count();
        assert_eq!(stores, 0);
    }
}
