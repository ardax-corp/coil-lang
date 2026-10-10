//! Label id space of an op buffer: the next free id, and remapping a
//! chunk's labels into a fresh range.

use std::collections::HashMap;

use super::super::op::{IlOp, Label};

/// Highest label id bound or targeted by `ops` (jumps and calls), or `0`.
pub(crate) fn max_code_label(ops: &[IlOp]) -> u32 {
    ops.iter().filter_map(code_label_id).max().unwrap_or(0)
}

fn code_label_id(op: &IlOp) -> Option<u32> {
    match op {
        IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) => Some(*id),
        IlOp::Jump { target, .. } | IlOp::Entry { target, .. } => Some(target.0),
        _ => None,
    }
}

/// Remap labels in `ops` into a fresh id space starting at `*next_label`.
///
/// Jump targets that this chunk binds remap locally. Targets still unbound
/// after [`super::super::module::absorb_trailing_labels`] may use
/// `prior_labels` (true cross-function jumps). CALL/CodePtr use Entry.
pub(crate) fn remap_label_space(
    ops: &[IlOp],
    next_label: &mut u32,
    prior_labels: &HashMap<u32, u32>,
) -> (Vec<IlOp>, HashMap<u32, u32>) {
    if ops.is_empty() {
        return (Vec::new(), HashMap::new());
    }
    let mut map = HashMap::<u32, u32>::new();
    for op in ops {
        if let IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) = op {
            map.entry(*id).or_insert_with(|| {
                let n = *next_label;
                *next_label = next_label.saturating_add(1);
                n
            });
        }
    }
    if map.is_empty() && prior_labels.is_empty() {
        return (ops.to_vec(), map);
    }
    let remapped = ops
        .iter()
        .map(|op| match op {
            IlOp::Label(Label(id)) => IlOp::Label(Label(map[id])),
            IlOp::JoinLabel(Label(id)) => IlOp::JoinLabel(Label(map[id])),
            IlOp::Jump {
                kind,
                target,
                loc,
                hint,
            } => IlOp::Jump {
                kind: *kind,
                target: Label(
                    map.get(&target.0)
                        .copied()
                        .or_else(|| prior_labels.get(&target.0).copied())
                        .unwrap_or(target.0),
                ),
                loc: *loc,
                hint: *hint,
            },
            IlOp::Entry {
                kind,
                arity,
                target,
                loc,
                ret_words,
            } => IlOp::Entry {
                kind: *kind,
                arity: *arity,
                // Local recursive calls only. Cross-function CALL/CodePtr
                // targets are patched from function entry labels after concat
                // (`prior` collides when two bodies reuse 0..n).
                target: Label(map.get(&target.0).copied().unwrap_or(target.0)),
                loc: *loc,
                ret_words: *ret_words,
            },
            other => other.clone(),
        })
        .collect();
    (remapped, map)
}
