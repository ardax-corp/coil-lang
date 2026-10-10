//! IL stream builder with symbolic label allocation.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use common::{Byte, DebugLoc};

use super::op::{EntryKind, FuseHint, IlJumpKind, IlOp, Label};

/// Error from IL finalize / lower.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IlError {
    /// A jump or entry targeted a label that was never bound.
    UnboundLabel(Label),
}

impl std::fmt::Display for IlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IlError::UnboundLabel(label) => {
                write!(f, "label {:?} was never bound", label)
            }
        }
    }
}

impl std::error::Error for IlError {}

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

    /// Structural access (insert / remove / reorder ops). Drops the code
    /// index, so prefer [`Self::ops_slice_mut`] for in-place rewrites.
    pub fn ops_mut(&mut self) -> &mut Vec<IlOp> {
        *self.code_index.get_mut() = CodeIndex::default();
        &mut self.ops
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

    /// Record a jump to `label` written in place ([`Self::ops_slice_mut`]).
    pub fn note_targeted(&mut self, label: Label) {
        self.targeted.insert(label.0);
    }

    /// Code offset of the raw op `raw` (the emitting ops before it).
    pub fn code_pos_of_raw(&self, raw: usize) -> usize {
        self.indexed().positions.partition_point(|&p| p < raw)
    }

    /// Move `ops[start..end]` to the end of the stream, behind a fresh bound
    /// `label` when there is one; the code index before `start` stays valid.
    pub fn move_to_end(&mut self, start: usize, end: usize, label: Option<Label>) {
        let mut end = end;
        if let Some(label) = label {
            self.bound.insert(label.0);
            self.ops.insert(start, IlOp::Label(label));
            end += 1;
        }
        self.ops[start..].rotate_left(end - start);
        let index = self.code_index.get_mut();
        if index.scanned > start {
            let keep = index.positions.partition_point(|&p| p < start);
            index.positions.truncate(keep);
            index.scanned = start;
        }
    }

    /// Raw index of the emitting op at code offset `pc`, if there is one.
    pub fn raw_index_of_code(&self, pc: usize) -> Option<usize> {
        self.indexed().positions.get(pc).copied()
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

    /// Where an op inserted at code offset `code_pos` goes: right after the
    /// op at `code_pos - 1`, so labels bound at `code_pos` follow it (the
    /// end when `code_pos` is past the last op).
    pub fn raw_insert_point(&self, code_pos: usize) -> usize {
        if code_pos == 0 {
            return 0;
        }
        self.raw_index_of_code(code_pos - 1)
            .map_or(self.ops.len(), |i| i + 1)
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

    /// Remove the last op, which must emit code.
    pub fn pop_last(&mut self) {
        debug_assert!(self.ops.last().is_some_and(IlOp::emits_code));
        self.ops.pop();
        let index = self.code_index.get_mut();
        if index.scanned > self.ops.len() {
            index.positions.retain(|&p| p < self.ops.len());
            index.scanned = self.ops.len();
        }
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

    /// Drop the label marker last pushed.
    pub fn pop_label(&mut self) {
        let Some(IlOp::Label(label) | IlOp::JoinLabel(label)) = self.ops.pop() else {
            panic!("pop_label: last op is not a label");
        };
        self.bound.remove(&label.0);
    }

    /// Insert a bound label marker at raw op index `raw_idx` (does not append).
    pub fn insert_bound_label_at(&mut self, raw_idx: usize, label: Label) {
        *self.code_index.get_mut() = CodeIndex::default();
        self.bound.insert(label.0);
        self.ops.insert(raw_idx, IlOp::Label(label));
    }

    pub fn emit_jump(&mut self, kind: IlJumpKind, target: Label) {
        self.emit_jump_at(kind, target, DebugLoc::unknown());
    }

    pub fn emit_jump_at(&mut self, kind: IlJumpKind, target: Label, loc: DebugLoc) {
        self.emit_jump_hinted(kind, target, loc, FuseHint::default());
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

    pub fn emit_entry(&mut self, kind: EntryKind, arity: u32, target: Label) {
        self.emit_entry_at(kind, arity, target, DebugLoc::unknown());
    }

    pub fn emit_entry_at(&mut self, kind: EntryKind, arity: u32, target: Label, loc: DebugLoc) {
        self.emit_entry_ret_at(kind, arity, target, loc, 1);
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

    pub fn push_const(&mut self, imm: i32) {
        self.push_op(IlOp::Const {
            imm,
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_return(&mut self) {
        self.push_op(IlOp::Return {
            loc: DebugLoc::unknown(),
            ret_words: 1,
        });
    }

    /// Two-slot `RETURN`: pops/pushes `[payload, tag]` instead of one word.
    pub fn push_return_two_word(&mut self) {
        self.push_op(IlOp::Return {
            loc: DebugLoc::unknown(),
            ret_words: 2,
        });
    }

    pub fn push_load(&mut self, slot: u32) {
        self.push_op(IlOp::Load {
            slot,
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_store_pop(&mut self, slot: u32) {
        self.push_op(IlOp::StorePop {
            slot,
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_pop(&mut self) {
        self.push_op(IlOp::Pop {
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_index(&mut self) {
        self.push_op(IlOp::Index {
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_index_unchecked(&mut self) {
        self.push_op(IlOp::IndexUnchecked {
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_array_pin(&mut self, slot: u32) {
        self.push_op(IlOp::ArrayPin {
            slot,
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_index_pin_unchecked(&mut self, slot: u32) {
        self.push_op(IlOp::IndexPinUnchecked {
            slot,
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_make_tuple(&mut self, arity: u32) {
        self.push_make_tuple_kinds(arity, 0);
    }

    /// `MakeTuple` with element word kinds (`common::pack_word_kinds`).
    pub fn push_make_tuple_kinds(&mut self, arity: u32, kinds: u8) {
        self.push_op(IlOp::MakeTuple {
            kinds,
            arity,
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_make_array(&mut self, arity: u32) {
        self.push_make_array_kind(arity, common::WORD_UNKNOWN);
    }

    /// `MakeArray` with the element word kind (`common::WORD_*`).
    pub fn push_make_array_kind(&mut self, arity: u32, elem_kind: u8) {
        self.push_op(IlOp::MakeArray {
            arity,
            elem_kind,
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_make_enum(&mut self, tag: u16, arity: u16) {
        self.push_make_enum_kinds(tag, arity, 0);
    }

    /// `MakeEnum` with payload word kinds (`common::pack_word_kinds`).
    pub fn push_make_enum_kinds(&mut self, tag: u16, arity: u16, kinds: u8) {
        self.push_op(IlOp::MakeEnum {
            kinds,
            tag,
            arity,
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_box_value(&mut self, tag: u32) {
        self.push_op(IlOp::BoxValue {
            tag,
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_unbox_value(&mut self, tag: u32) {
        self.push_op(IlOp::UnboxValue {
            tag,
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_load_field(&mut self, index: u32) {
        self.push_op(IlOp::LoadField {
            index,
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_get_field(&mut self) {
        self.push_op(IlOp::GetField {
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_set_field(&mut self) {
        self.push_op(IlOp::SetField {
            index: None,
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_set_field_slot(&mut self, index: u32) {
        self.push_op(IlOp::SetField {
            index: Some(index),
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_host_invoke(&mut self, arity: u32) {
        self.push_host_invoke_layout(arity, common::HOST_ENUM_LAYOUT_BOXED);
    }

    pub fn push_host_invoke_layout(&mut self, arity: u32, layout: u32) {
        self.push_op(IlOp::HostInvoke {
            arity,
            layout: layout as u8,
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_print(&mut self) {
        self.push_op(IlOp::Print {
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_const_pool(&mut self, idx: u32) {
        self.push_op(IlOp::ConstPool {
            idx,
            loc: DebugLoc::unknown(),
        });
    }

    pub fn push_string(&mut self, idx: u32) {
        self.push_op(IlOp::String {
            idx,
            loc: DebugLoc::unknown(),
        });
    }

    /// Move `other`'s ops onto the end, remapping its labels to fresh ids —
    /// except the `Entry` ops at `keep` (indices into `other`), which already
    /// target labels of this namespace and keep their ids.
    pub fn append(&mut self, other: &mut IlBuilder, keep: &[usize]) -> BTreeMap<u32, u32> {
        // Merge label id spaces: remap other's labels to fresh ids.
        if other.ops.is_empty() {
            return BTreeMap::new();
        }
        let keep: std::collections::HashSet<usize> = keep.iter().copied().collect();
        let mut remap: BTreeMap<u32, u32> = BTreeMap::new();
        let mut map_label = |id: u32, me: &mut Self| -> u32 {
            *remap.entry(id).or_insert_with(|| {
                let n = me.next_label_id;
                me.next_label_id += 1;
                n
            })
        };
        for (i, op) in other.ops.drain(..).enumerate() {
            if keep.contains(&i)
                && let IlOp::Entry { target, .. } = &op
            {
                self.targeted.insert(target.0);
                self.ops.push(op);
                continue;
            }
            match op {
                IlOp::Label(Label(id)) => {
                    let nid = map_label(id, self);
                    self.bound.insert(nid);
                    self.ops.push(IlOp::Label(Label(nid)));
                }
                IlOp::JoinLabel(Label(id)) => {
                    let nid = map_label(id, self);
                    self.bound.insert(nid);
                    self.ops.push(IlOp::JoinLabel(Label(nid)));
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

    /// Ensure every targeted label was bound.
    #[cfg(test)]
    pub fn finalize_labels(&self) -> Result<(), IlError> {
        for id in &self.targeted {
            if !self.bound.contains(id) {
                return Err(IlError::UnboundLabel(Label(*id)));
            }
        }
        Ok(())
    }

    /// Splice `inserted` before the first op at logical code index `code_pos`
    /// (counting only emitting ops). Used for static-init insertion.
    pub fn splice_code_at(&mut self, code_pos: usize, mut inserted: IlBuilder) {
        let raw_idx = self.raw_insert_point(code_pos);
        *self.code_index.get_mut() = CodeIndex::default();
        let mut chunk = std::mem::take(&mut inserted.ops);
        // Remap labels from inserted into our namespace.
        let mut remap: BTreeMap<u32, u32> = BTreeMap::new();
        for op in &mut chunk {
            match op {
                IlOp::Label(Label(id))
                | IlOp::JoinLabel(Label(id))
                | IlOp::Jump {
                    target: Label(id), ..
                }
                | IlOp::Entry {
                    target: Label(id), .. } => {
                    let nid = *remap.entry(*id).or_insert_with(|| {
                        let n = self.next_label_id;
                        self.next_label_id += 1;
                        n
                    });
                    *id = nid;
                }
                _ => {}
            }
        }
        for op in &chunk {
            if let IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) = op {
                self.bound.insert(*id);
            }
            if let IlOp::Jump {
                target: Label(id), ..
            }
            | IlOp::Entry {
                target: Label(id), .. } = op
            {
                self.targeted.insert(*id);
            }
        }
        self.ops.splice(raw_idx..raw_idx, chunk);
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

    #[test]
    fn code_ranges_match_a_scan() {
        let il = sample();
        assert_eq!(il.code_len(), 3);
        for start in 0..5 {
            for end in start..6 {
                let range: Vec<usize> = il.raw_range_of_code(start, end).collect();
                assert_eq!(range, scanned(&il, start, end), "[{start}, {end})");
            }
        }
        assert_eq!(il.raw_insert_point(0), 0);
        assert_eq!(il.raw_insert_point(2), 4);
        assert_eq!(il.raw_insert_point(9), il.raw_len());
    }

}
