//! Natural loops over a `Vec<IlOp>` (the tier choice's loop-aware cost).

use super::op::{IlJumpKind, IlOp, Label};

/// Natural loop identified by an unconditional back-edge to a header label.
#[derive(Clone, Debug)]
pub(crate) struct NaturalLoop {
    pub(crate) header: usize,
    /// Index of back-edge `Jump` (unconditional) to header.
    pub(crate) latch: usize,
    pub(crate) header_label: Label,
}

/// IL is module-flat; labels reuse per function. Scope lookups to the function
/// containing `idx` (ops since the previous `Return`).
pub(crate) fn il_function_start(ops: &[IlOp], idx: usize) -> usize {
    for i in (0..idx).rev() {
        if matches!(ops[i], IlOp::Return { .. }) {
            return i + 1;
        }
    }
    0
}

fn resolve_label_before(ops: &[IlOp], before: usize, target: Label) -> Option<usize> {
    let start = il_function_start(ops, before);
    for i in (start..before).rev() {
        if matches!(&ops[i], IlOp::Label(l) | IlOp::JoinLabel(l) if *l == target) {
            return Some(i);
        }
    }
    None
}

/// Unconditional back-edges whose target label binds earlier in the same function.
pub(crate) fn find_natural_loops(ops: &[IlOp]) -> Vec<NaturalLoop> {
    let mut out = Vec::new();
    for (i, op) in ops.iter().enumerate() {
        let IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target,
            ..
        } = op
        else {
            continue;
        };
        let Some(h) = resolve_label_before(ops, i, *target) else {
            continue;
        };
        if h >= i {
            continue;
        }
        out.push(NaturalLoop {
            header: h,
            latch: i,
            header_label: *target,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::il::op::IlOp;

    fn loc() -> common::DebugLoc {
        common::DebugLoc::unknown()
    }

    #[test]
    fn finds_unconditional_back_edge() {
        let ops = vec![
            IlOp::Label(Label(0)),
            IlOp::Const {
                imm: 1,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
        ];
        let loops = find_natural_loops(&ops);
        assert_eq!(loops.len(), 1);
        assert_eq!(loops[0].header, 0);
        assert_eq!(loops[0].latch, 2);
    }
}
