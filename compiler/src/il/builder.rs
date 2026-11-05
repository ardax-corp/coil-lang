//! IL stream builder with symbolic label allocation.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use common::{Byte, DebugLoc};

#[cfg(test)]
use super::op::{FuseHint, IlJumpKind};
use super::op::{EntryKind, IlOp, Label};

/// Raw index of every code-emitting op in `ops[..scanned]`, extended lazily
/// as ops are pushed. Maps a code offset (PC) to its op in O(1) instead of
/// a scan from op 0 (#608).
#[derive(Clone, Default)]
struct CodeIndex {
    scanned: usize,
    positions: Vec<usize>,
}

/// Accumulates stack IL with symbolic jump/entry targets.
#[derive(Clone, Default)]
pub struct IlBuilder {
    ops: Vec<IlOp>,
    next_label_id: u32,
    /// Labels that were targeted by a jump/entry.
    targeted: BTreeSet<u32>,
    /// Labels that have been bound at least once.
    bound: BTreeSet<u32>,
    /// Valid for `ops[..scanned]`; reset by structural edits.
    code_index: RefCell<CodeIndex>,
}

impl IlBuilder {
    #[cfg(test)]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn ops(&self) -> &[IlOp] {
        &self.ops
    }

    /// In-place access to the ops (locations, site tags, an `Entry` for an
    /// absolute CALL byte). The caller must not change which ops emit code.
    pub fn ops_slice_mut(&mut self) -> &mut [IlOp] {
        &mut self.ops
    }

    /// Index every emitting op pushed since the last query.
    fn indexed(&self) -> std::cell::Ref<'_, CodeIndex> {
        {
            let mut index = self.code_index.borrow_mut();
            let from = index.scanned;
            if from < self.ops.len() {
                for (i, op) in self.ops[from..].iter().enumerate() {
                    if op.emits_code() {
                        index.positions.push(from + i);
                    }
                }
                index.scanned = self.ops.len();
            }
        }
        self.code_index.borrow()
    }

    /// Raw ops range holding code offsets `[start, end)` plus the labels
    /// bound at those offsets: from just after the op at `start - 1` to just
    /// after the op at `end - 1` (labels at offset `end` excluded; past the
    /// last op, trailing labels are included only when `end` exceeds it).
    pub fn raw_range_of_code(&self, start: usize, end: usize) -> std::ops::Range<usize> {
        let index = self.indexed();
        let total = index.positions.len();
        let after = |pc: usize| -> usize {
            if pc == 0 {
                0
            } else if pc <= total {
                index.positions[pc - 1] + 1
            } else {
                self.ops.len()
            }
        };
        let lo = after(start);
        let hi = if end > total { self.ops.len() } else { after(end) };
        lo..hi.max(lo)
    }

    pub fn clear(&mut self) {
        *self.code_index.get_mut() = CodeIndex::default();
        self.ops.clear();
        self.next_label_id = 0;
        self.targeted.clear();
        self.bound.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Number of code-emitting ops (labels excluded). Useful for spans.
    pub fn code_len(&self) -> usize {
        self.indexed().positions.len()
    }

    /// Total IL items including label markers.
    pub fn raw_len(&self) -> usize {
        self.ops.len()
    }

    pub fn fresh_label(&mut self) -> Label {
        let id = self.next_label_id;
        self.next_label_id += 1;
        Label(id)
    }

    /// Bind `label` at the current stream position (next emitting op).
    /// Idempotent: a later bind wins at lower time.
    pub fn bind_label(&mut self, label: Label) {
        self.bound.insert(label.0);
        self.ops.push(IlOp::Label(label));
    }

    /// `EntryKind::Call` with an explicit return width (`1` or `2` words).
    /// Every other kind should keep `ret_words = 1`.
    pub fn emit_entry_ret_at(
        &mut self,
        kind: EntryKind,
        arity: u32,
        target: Label,
        loc: DebugLoc,
        ret_words: u32,
    ) {
        self.targeted.insert(target.0);
        self.ops.push(IlOp::Entry {
            kind,
            arity,
            target,
            loc,
            ret_words,
        });
    }

    pub fn push_byte(&mut self, byte: Byte) {
        self.ops.push(IlOp::byte(byte));
    }

    /// Append a typed IL op (prefer over [`Self::push_byte`] for hot-set ops).
    pub fn push_op(&mut self, op: IlOp) {
        self.ops.push(op);
    }

    pub fn push_return(&mut self) {
        self.push_op(IlOp::Return {
            loc: DebugLoc::unknown(),
            ret_words: 1,
        });
    }

    /// Move `other`'s ops onto the end, remapping its labels to fresh ids.
    pub fn append(&mut self, other: &mut IlBuilder) -> BTreeMap<u32, u32> {
        // Merge label id spaces: remap other's labels to fresh ids.
        if other.ops.is_empty() {
            return BTreeMap::new();
        }
        let mut remap: BTreeMap<u32, u32> = BTreeMap::new();
        let mut map_label = |id: u32, me: &mut Self| -> u32 {
            *remap.entry(id).or_insert_with(|| {
                let n = me.next_label_id;
                me.next_label_id += 1;
                n
            })
        };
        for op in other.ops.drain(..) {
            match op {
                IlOp::Label(Label(id)) => {
                    let nid = map_label(id, self);
                    self.bound.insert(nid);
                    self.ops.push(IlOp::Label(Label(nid)));
                }
                IlOp::Jump {
                    kind,
                    target,
                    loc,
                    hint,
                } => {
                    let nid = map_label(target.0, self);
                    self.targeted.insert(nid);
                    self.ops.push(IlOp::Jump {
                        kind,
                        target: Label(nid),
                        loc,
                        hint,
                    });
                }
                IlOp::Entry {
                    kind,
                    arity,
                    target,
                    loc,
                    ret_words,
                } => {
                    let nid = map_label(target.0, self);
                    self.targeted.insert(nid);
                    self.ops.push(IlOp::Entry {
                        kind,
                        arity,
                        target: Label(nid),
                        loc,
                        ret_words,
                    });
                }
                other_op => self.ops.push(other_op),
            }
        }
        other.clear();
        remap
    }

    pub fn push_prologue_jmp(&mut self) {
        self.ops.push(IlOp::PrologueJmp {
            loc: DebugLoc::unknown(),
        });
    }

}

// Test fixtures build their IL with these.
#[cfg(test)]
impl IlBuilder {
    pub fn emit_entry_at(&mut self, kind: EntryKind, arity: u32, target: Label, loc: DebugLoc) {
        self.emit_entry_ret_at(kind, arity, target, loc, 1);
    }

    pub fn push_store_pop(&mut self, slot: u32) {
        self.push_op(IlOp::StorePop {
            slot,
            loc: DebugLoc::unknown(),
        });
    }

    pub fn emit_jump_at(&mut self, kind: IlJumpKind, target: Label, loc: DebugLoc) {
        self.emit_jump_hinted(kind, target, loc, FuseHint::default());
    }

    pub fn push_load(&mut self, slot: u32) {
        self.push_op(IlOp::Load {
            slot,
            loc: DebugLoc::unknown(),
        });
    }

    pub fn emit_entry(&mut self, kind: EntryKind, arity: u32, target: Label) {
        self.emit_entry_at(kind, arity, target, DebugLoc::unknown());
    }

    pub fn push_const(&mut self, imm: i32) {
        self.push_op(IlOp::Const {
            imm,
            loc: DebugLoc::unknown(),
        });
    }

    pub fn emit_jump_hinted(
        &mut self,
        kind: IlJumpKind,
        target: Label,
        loc: DebugLoc,
        hint: FuseHint,
    ) {
        self.targeted.insert(target.0);
        self.ops.push(IlOp::jump_hinted(kind, target, loc, hint));
    }

    pub fn emit_jump(&mut self, kind: IlJumpKind, target: Label) {
        self.emit_jump_at(kind, target, DebugLoc::unknown());
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    /// `[L0] a [L1] b [L2] c [L3]`: three emitting ops, a label at each PC.
    fn sample() -> IlBuilder {
        let mut il = IlBuilder::new();
        for k in 0..3 {
            let l = il.fresh_label();
            il.bind_label(l);
            il.push_const(k);
        }
        let l = il.fresh_label();
        il.bind_label(l);
        il
    }

    /// The scan the index replaces: labels at PC `i` in `[start, end)` plus
    /// emitting ops `[start, end)`.
    fn scanned(il: &IlBuilder, start: usize, end: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut pc = 0;
        for (i, op) in il.ops().iter().enumerate() {
            if op.emits_code() {
                if pc >= end {
                    break;
                }
                if pc >= start {
                    out.push(i);
                }
                pc += 1;
            } else if pc >= start && pc < end {
                out.push(i);
            }
        }
        out
    }

}
