//! Bytecode buffer backed by stack IL during emit; lowered at finalize.

use std::collections::HashMap;

use common::{Byte, DebugLoc, Instruction};

use super::lower::lower_module_inner;
use super::{EntryKind, IlBuilder, IlFunc, IlJumpKind, IlModule, IlOp, Label, Lowered};

/// Compile-time code buffer: IL during emit, `Vec<Byte>` after lower.
#[derive(Clone, Default)]
pub struct CodeBuf {
    il: IlBuilder,
    lowered: Option<Vec<Byte>>,
    lowered_locs: Option<Vec<DebugLoc>>,
    /// Logical code index → entry label from [`Self::bind_fresh_entry`].
    /// Used to rewrite packed CALL/CodePtr Bytes into [`IlOp::Entry`].
    entry_at_offset: HashMap<usize, Label>,
    /// Per-function spans recorded at function finalize (flat buffer).
    funcs: Vec<IlFunc>,
    /// IL opt preset used at [`Self::lower_in_place`].
    opt_options: super::opt::OptimizeOptions,
}

impl CodeBuf {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_opt_options(&mut self, opts: super::opt::OptimizeOptions) {
        self.opt_options = opts;
    }

    pub fn il(&self) -> &IlBuilder {
        &self.il
    }

    pub fn il_mut(&mut self) -> &mut IlBuilder {
        // Error-recovery may mutate IL after a prior lower.
        self.lowered = None;
        self.lowered_locs = None;
        &mut self.il
    }

    pub fn push(&mut self, b: Byte) {
        // Error-recovery paths may emit after a failed finalize/lower.
        self.lowered = None;
        self.lowered_locs = None;
        if let Some((kind, arity, label, ret_words)) = self.entry_from_abs_byte(b) {
            self.il
                .emit_entry_ret_at(kind, arity, label, DebugLoc::unknown(), ret_words);
        } else {
            self.il.push_byte(b);
        }
    }

    fn invalidate_lowered(&mut self) {
        self.lowered = None;
        self.lowered_locs = None;
    }

    /// Append a typed IL op (prefer for hot-set Const/Return/Load/…).
    pub fn push_op(&mut self, op: IlOp) {
        self.invalidate_lowered();
        self.il.push_op(op);
    }

    pub fn push_const(&mut self, imm: i32) {
        self.invalidate_lowered();
        self.il.push_const(imm);
    }

    pub fn push_return(&mut self) {
        self.invalidate_lowered();
        self.il.push_return();
    }

    pub fn push_return_at(&mut self, loc: DebugLoc) {
        self.invalidate_lowered();
        self.il.push_return_at(loc);
    }

    /// Two-slot `RETURN`: pops/pushes `[payload, tag]` instead of one word.
    pub fn push_return_two_word(&mut self) {
        self.invalidate_lowered();
        self.il.push_return_two_word();
    }

    pub fn push_load(&mut self, slot: u32) {
        self.invalidate_lowered();
        self.il.push_load(slot);
    }

    pub fn push_store_pop(&mut self, slot: u32) {
        self.invalidate_lowered();
        self.il.push_store_pop(slot);
    }

    pub fn push_pop(&mut self) {
        self.invalidate_lowered();
        self.il.push_pop();
    }

    pub fn push_index(&mut self) {
        self.invalidate_lowered();
        self.il.push_index();
    }

    pub fn push_index_unchecked(&mut self) {
        self.invalidate_lowered();
        self.il.push_index_unchecked();
    }

    pub fn push_array_pin(&mut self, slot: u32) {
        self.invalidate_lowered();
        self.il.push_array_pin(slot);
    }

    pub fn push_index_pin_unchecked(&mut self, slot: u32) {
        self.invalidate_lowered();
        self.il.push_index_pin_unchecked(slot);
    }

    pub fn push_store_index_unchecked(&mut self) {
        self.invalidate_lowered();
        self.il.push_store_index_unchecked();
    }

    pub fn push_make_tuple(&mut self, arity: u32) {
        self.invalidate_lowered();
        self.il.push_make_tuple(arity);
    }

    pub fn push_make_array(&mut self, arity: u32) {
        self.invalidate_lowered();
        self.il.push_make_array(arity);
    }

    pub fn push_make_enum(&mut self, tag: u16, arity: u16) {
        self.invalidate_lowered();
        self.il.push_make_enum(tag, arity);
    }

    pub fn push_box_value(&mut self, tag: u32) {
        self.invalidate_lowered();
        self.il.push_box_value(tag);
    }

    pub fn push_unbox_value(&mut self, tag: u32) {
        self.invalidate_lowered();
        self.il.push_unbox_value(tag);
    }

    pub fn push_load_field(&mut self, index: u32) {
        self.invalidate_lowered();
        self.il.push_load_field(index);
    }

    pub fn push_get_field(&mut self) {
        self.invalidate_lowered();
        self.il.push_get_field();
    }

    pub fn push_set_field(&mut self) {
        self.invalidate_lowered();
        self.il.push_set_field();
    }

    pub fn push_set_field_slot(&mut self, index: u32) {
        self.invalidate_lowered();
        self.il.push_set_field_slot(index);
    }

    pub fn push_host_invoke(&mut self, arity: u32) {
        self.invalidate_lowered();
        self.il.push_host_invoke(arity);
    }

    pub fn push_print(&mut self) {
        self.invalidate_lowered();
        self.il.push_print();
    }

    pub fn push_const_pool(&mut self, idx: u32) {
        self.invalidate_lowered();
        self.il.push_const_pool(idx);
    }

    pub fn push_string(&mut self, idx: u32) {
        self.invalidate_lowered();
        self.il.push_string(idx);
    }

    /// If `b` is a call-like op targeting a known entry PC, return Entry parts
    /// `(kind, arity, target, ret_words)`.
    fn entry_from_abs_byte(&self, b: Byte) -> Option<(EntryKind, u32, Label, u32)> {
        match *b.bytecode() {
            Instruction::CALL => {
                let (arity, target) = b.call_parts();
                let label = *self.entry_at_offset.get(&target)?;
                Some((EntryKind::Call, arity as u32, label, b.call_ret_words()))
            }
            Instruction::TailCall => {
                let (arity, target) = b.call_parts();
                let label = *self.entry_at_offset.get(&target)?;
                Some((EntryKind::TailCall, arity as u32, label, 1))
            }
            Instruction::MakeCoro => {
                let (arity, target) = b.call_parts();
                let label = *self.entry_at_offset.get(&target)?;
                Some((EntryKind::MakeCoro, arity as u32, label, 1))
            }
            Instruction::CodePtr => {
                let target = b.operand_u32() as usize;
                let label = *self.entry_at_offset.get(&target)?;
                Some((EntryKind::CodePtr, 0, label, 1))
            }
            Instruction::MakePolyFn => {
                let target = b.operand_u32() as usize;
                let label = *self.entry_at_offset.get(&target)?;
                Some((EntryKind::MakePolyFn, 0, label, 1))
            }
            _ => None,
        }
    }

    /// Merge `other`'s IL onto this buffer. Labels in `other` are remapped.
    /// Packed CALL/CodePtr Bytes in the suffix are rewritten using this
    /// buffer's entry map (local fragments have an empty map, so `push`
    /// would not rewrite).
    pub fn append(&mut self, other: &mut CodeBuf) {
        if other.il.is_empty() && other.entry_at_offset.is_empty() && other.funcs.is_empty() {
            other.clear();
            return;
        }
        self.invalidate_lowered();
        let base = self.len();
        let remap = self.il.append(&mut other.il);
        for (pc, label) in other.entry_at_offset.drain() {
            let nid = remap.get(&label.0).copied().unwrap_or(label.0);
            self.entry_at_offset.insert(pc + base, Label(nid));
        }
        for mut f in other.funcs.drain(..) {
            f.code_start += base;
            f.code_end += base;
            if let Some(Label(id)) = f.entry {
                f.entry = Some(Label(remap.get(&id).copied().unwrap_or(id)));
            }
            self.funcs.push(f);
        }
        self.rewrite_abs_entries_from(base);
        other.clear();
    }

    fn rewrite_abs_entries_from(&mut self, from_code: usize) {
        let mut emitting = 0usize;
        let mut rewrites = Vec::new();
        for (i, op) in self.il.ops().iter().enumerate() {
            if !op.emits_code() {
                continue;
            }
            if emitting >= from_code {
                if let IlOp::Byte { byte, loc } = op {
                    if let Some((kind, arity, label, ret_words)) = self.entry_from_abs_byte(*byte)
                    {
                        rewrites.push((i, kind, arity, label, *loc, ret_words));
                    }
                }
            }
            emitting += 1;
        }
        for (i, kind, arity, target, loc, ret_words) in rewrites {
            self.il.ops_mut()[i] = IlOp::Entry {
                kind,
                arity,
                target,
                loc,
                ret_words,
            };
        }
    }

    pub fn len(&self) -> usize {
        match &self.lowered {
            Some(v) => v.len(),
            None => self.il.code_len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&mut self) {
        self.il.clear();
        self.lowered = None;
        self.lowered_locs = None;
        self.entry_at_offset.clear();
        self.funcs.clear();
    }

    /// Record a function's emitting span, optional entry label, and entry SP.
    #[cfg(test)]
    pub fn record_func(
        &mut self,
        name: impl Into<String>,
        entry: Option<Label>,
        code_start: usize,
        code_end: usize,
    ) {
        self.record_func_with_sp(name, entry, code_start, code_end, 0);
    }

    pub fn record_func_with_sp(
        &mut self,
        name: impl Into<String>,
        entry: Option<Label>,
        code_start: usize,
        code_end: usize,
        entry_sp: u32,
    ) {
        self.funcs.push(IlFunc::with_entry_sp(
            name, entry, code_start, code_end, entry_sp,
        ));
    }

    pub fn funcs(&self) -> &[IlFunc] {
        &self.funcs
    }

    /// Keep only function records matching `pred` (treeshake after deletes).
    pub fn retain_funcs(&mut self, mut pred: impl FnMut(&IlFunc) -> bool) {
        self.funcs.retain(|f| pred(f));
    }

    /// Snapshot of entry-label bindings (emitting PC → label).
    pub fn entry_labels(&self) -> impl Iterator<Item = (usize, Label)> + '_ {
        self.entry_at_offset.iter().map(|(&pc, &l)| (pc, l))
    }

    /// Remove raw IL ops in `[raw_start, raw_end)`.
    pub fn remove_raw_range(&mut self, raw_start: usize, raw_end: usize) {
        self.invalidate_lowered();
        if raw_start >= raw_end || raw_end > self.il.ops().len() {
            return;
        }
        self.il.ops_mut().drain(raw_start..raw_end);
    }

    /// After deleting emitting ops `[start, end)`, drop entry PCs in that
    /// range and subtract `end - start` from all greater PCs.
    pub fn shift_entry_pcs_after_delete(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let delta = end - start;
        let mut next = HashMap::with_capacity(self.entry_at_offset.len());
        for (pc, label) in self.entry_at_offset.drain() {
            if pc < start {
                next.insert(pc, label);
            } else if pc >= end {
                next.insert(pc - delta, label);
            }
        }
        self.entry_at_offset = next;
    }

    /// Shrink recorded func spans after deleting emitting ops `[start, end)`.
    ///
    /// Callers must drop overlapping records first; survivors are only those
    /// entirely before `start` or entirely at/after `end`.
    pub fn shrink_func_spans_after_delete(&mut self, start: usize, end: usize) {
        if start >= end {
            return;
        }
        let delta = end - start;
        for f in &mut self.funcs {
            if f.code_end <= start {
                continue;
            }
            debug_assert!(
                f.code_start >= end,
                "overlapping IlFunc must be removed before shrink"
            );
            f.code_start -= delta;
            f.code_end -= delta;
        }
    }

    /// Drop func records whose span overlaps `[start, end)`.
    pub fn remove_func_spans_overlapping(&mut self, start: usize, end: usize) {
        self.funcs
            .retain(|f| f.code_end <= start || f.code_start >= end);
    }

    /// Shift recorded [`IlFunc`] emitting spans after a splice at `threshold`.
    pub fn bump_func_spans(&mut self, threshold: usize, delta: usize) {
        if delta == 0 {
            return;
        }
        for f in &mut self.funcs {
            if f.code_start >= threshold {
                f.code_start += delta;
            }
            if f.code_end >= threshold {
                f.code_end += delta;
            }
        }
    }

    pub fn fresh_label(&mut self) -> Label {
        self.il.fresh_label()
    }

    pub fn bind_label(&mut self, label: Label) {
        self.il.bind_label(label);
    }

    pub fn bind_join_label(&mut self, label: Label) {
        self.il.bind_join_label(label);
    }

    /// Bind a fresh label at the current emit position (fn / lambda / thunk entry).
    /// Labels do not advance [`Self::len`], so absolute PC tables stay aligned.
    /// Records the binding so later packed CALL/CodePtr Bytes rewrite to Entry.
    pub fn bind_fresh_entry(&mut self) -> Label {
        let pc = self.len();
        let label = self.fresh_label();
        self.bind_label(label);
        self.entry_at_offset.insert(pc, label);
        label
    }

    /// Bind a label previously allocated by [`Self::fresh_label`] at the current
    /// PC so earlier `Entry` ops can target a method that is compiled later.
    pub fn bind_reserved_entry(&mut self, label: Label) {
        let pc = self.len();
        self.bind_label(label);
        self.entry_at_offset.insert(pc, label);
    }

    /// Look up the entry label bound at logical code index `pc`, if any.
    pub fn entry_label_for_offset(&self, pc: usize) -> Option<Label> {
        self.entry_at_offset.get(&pc).copied()
    }

    /// Return an existing label bound at logical code index `code_pos`, or insert
    /// and bind a fresh one. Used by static-init JMP → `main` reconciliation.
    pub fn entry_label_at(&mut self, code_pos: usize) -> Label {
        let mut emitting = 0usize;
        let mut raw_idx = self.il.raw_len();
        let mut existing: Option<Label> = None;
        for (i, op) in self.il.ops().iter().enumerate() {
            if emitting == code_pos {
                if let IlOp::Label(l) | IlOp::JoinLabel(l) = op {
                    existing = Some(*l);
                } else {
                    raw_idx = i;
                }
                break;
            }
            if op.emits_code() {
                emitting += 1;
            }
        }
        if let Some(l) = existing {
            return l;
        }
        let label = self.il.fresh_label();
        self.il.insert_bound_label_at(raw_idx, label);
        self.entry_at_offset.entry(code_pos).or_insert(label);
        label
    }

    pub fn emit_entry(&mut self, kind: EntryKind, arity: u32, target: Label) {
        self.il.emit_entry(kind, arity, target);
    }

    pub fn push_prologue_jmp(&mut self) {
        self.il.push_prologue_jmp();
    }

    /// Splice another buffer's IL before logical code index `code_pos`,
    /// remapping labels into this buffer's namespace.
    pub fn splice_buf_at(&mut self, code_pos: usize, other: CodeBuf) {
        self.invalidate_lowered();
        self.il.splice_code_at(code_pos, other.il);
    }

    /// Move IL ops `[raw_start..]` to logical code index `code_pos` without
    /// remapping labels (same buffer namespace).
    pub fn move_raw_suffix_to_code_pos(&mut self, raw_start: usize, code_pos: usize) {
        self.invalidate_lowered();
        let suffix: Vec<IlOp> = self.il.ops_mut().drain(raw_start..).collect();
        if suffix.is_empty() {
            return;
        }
        let mut emitting = 0usize;
        let mut raw_idx = self.il.raw_len();
        for (i, op) in self.il.ops().iter().enumerate() {
            if emitting == code_pos {
                raw_idx = i;
                break;
            }
            if op.emits_code() {
                emitting += 1;
            }
        }
        if emitting < code_pos {
            raw_idx = self.il.raw_len();
        }
        self.il.ops_mut().splice(raw_idx..raw_idx, suffix);
    }

    pub fn insert_jump_at(&mut self, code_pos: usize, target: Label) {
        let mut emitting = 0usize;
        let mut raw_idx = self.il.raw_len();
        for (i, op) in self.il.ops().iter().enumerate() {
            if emitting == code_pos {
                raw_idx = i;
                break;
            }
            if op.emits_code() {
                emitting += 1;
            }
        }
        self.il.ops_mut().insert(
            raw_idx,
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target,
                loc: DebugLoc::unknown(),
                hint: Default::default(),
            },
        );
    }

    /// Rebuild an owning [`IlModule`] from the flat emit stream and lower once.
    pub fn lower_in_place(&mut self, pool: &mut Vec<u64>) -> Lowered {
        self.lower_in_place_inner(pool, false)
    }

    /// Like [`Self::lower_in_place`], keeping post-opt pre-fuse ops on [`Lowered`].
    pub(crate) fn lower_in_place_capturing(&mut self, pool: &mut Vec<u64>) -> Lowered {
        self.lower_in_place_inner(pool, true)
    }

    fn lower_in_place_inner(&mut self, pool: &mut Vec<u64>, capture_ops: bool) -> Lowered {
        let mut module = IlModule::from_flat(self.il.ops(), &self.funcs)
            .with_entries(self.entry_at_offset.clone());
        let lowered = lower_module_inner(&mut module, pool, capture_ops, &self.opt_options)
            .unwrap_or_else(|e| panic!("{e}"));
        self.lowered = Some(lowered.bytecode.clone());
        self.lowered_locs = Some(lowered.debug_locs.clone());
        lowered
    }

    pub fn as_slice(&self) -> &[Byte] {
        self.lowered.as_deref().unwrap_or(&[])
    }

    pub fn clone_bytes(&self) -> Vec<Byte> {
        self.lowered.clone().unwrap_or_default()
    }

    pub fn ops(&self) -> &[IlOp] {
        self.il.ops()
    }

    /// Truncate to `code_len` emitting ops. Labels bound at PC `code_len`
    /// (entry labels for the next instruction) are preserved; emitting ops
    /// at that PC and beyond are dropped.
    pub fn truncate(&mut self, code_len: usize) {
        assert!(self.lowered.is_none());
        let mut emitting = 0usize;
        let mut keep = 0usize;
        for (i, op) in self.il.ops().iter().enumerate() {
            if op.emits_code() {
                if emitting == code_len {
                    break;
                }
                emitting += 1;
                keep = i + 1;
            } else if emitting > code_len {
                break;
            } else {
                // Keep labels/markers at PCs `<= code_len` (incl. entry binds).
                keep = i + 1;
            }
        }
        self.il.ops_mut().truncate(keep);
        // Keep entries bound at `code_len` (next-emit PC). `discard_compile` of
        // a const-`if` condition often truncates back to a function's entry PC;
        // `pc < code_len` would drop that bind and leave later CALLs as stale
        // absolute PCs (breaks under BinSlotImm fusion).
        self.entry_at_offset.retain(|&pc, _| pc <= code_len);
    }

    /// Plain bytes in the emitting-op range `[start, end)` (labels skipped).
    /// Jump/Entry ops are omitted from the returned vec — callers that need
    /// a faithful body copy must judge candidacy via [`Self::code_slice_ops`].
    pub fn code_slice_bytes(&self, start: usize, end: usize) -> Vec<Byte> {
        let mut out = Vec::new();
        let mut i = 0usize;
        for op in self.il.ops() {
            if let Some(b) = op.as_plain_byte() {
                if i >= start && i < end {
                    out.push(b);
                }
                i += 1;
                if i >= end {
                    break;
                }
            } else if op.emits_code() {
                i += 1;
                if i >= end {
                    break;
                }
            }
        }
        out
    }

    /// Emitting ops in `[start, end)` (labels skipped; Jump/Entry included).
    pub fn code_slice_ops(&self, start: usize, end: usize) -> Vec<super::IlOp> {
        let mut out = Vec::new();
        let mut i = 0usize;
        for op in self.il.ops() {
            if !op.emits_code() {
                continue;
            }
            if i >= end {
                break;
            }
            if i >= start {
                out.push(op.clone());
            }
            i += 1;
        }
        out
    }

    /// Ops in emitting range `[start, end)`, including [`IlOp::Label`] markers
    /// that sit at those emitting positions (needed to copy jump diamonds).
    pub fn code_slice_raw_ops(&self, start: usize, end: usize) -> Vec<super::IlOp> {
        let mut out = Vec::new();
        let mut i = 0usize;
        for op in self.il.ops() {
            if matches!(op, super::IlOp::Label(_) | IlOp::JoinLabel(_)) {
                if i >= start && i < end {
                    out.push(op.clone());
                }
                continue;
            }
            if !op.emits_code() {
                continue;
            }
            if i >= end {
                break;
            }
            if i >= start {
                out.push(op.clone());
            }
            i += 1;
        }
        out
    }

    /// Shift [`Self::entry_at_offset`] keys after a splice that inserts `delta`
    /// emitting ops at `threshold`. Entry ops themselves are symbolic and need
    /// no rewrite; leftover abs CALL/CodePtr Bytes (missing fn CodePtr 0) are
    /// below any real entry PC and are left untouched.
    pub fn bump_absolute_entry_targets(&mut self, threshold: usize, delta: usize) {
        if delta == 0 {
            return;
        }
        let mut next = HashMap::with_capacity(self.entry_at_offset.len());
        for (pc, label) in self.entry_at_offset.drain() {
            let pc = if pc >= threshold { pc + delta } else { pc };
            next.insert(pc, label);
        }
        self.entry_at_offset = next;
    }

    pub fn last_byte(&self) -> Option<Byte> {
        for op in self.il.ops().iter().rev() {
            if let Some(b) = op.as_plain_byte() {
                return Some(b);
            }
            if op.emits_code() {
                return None;
            }
        }
        None
    }

    /// Remove the last emitting op (labels after it are left in place).
    pub fn pop_last_emitting(&mut self) -> Option<IlOp> {
        self.invalidate_lowered();
        let ops = self.il.ops_mut();
        let idx = ops.iter().rposition(|op| op.emits_code())?;
        Some(ops.remove(idx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::Instruction;

    #[test]
    fn truncate_keeps_entry_bound_at_code_len() {
        let mut buf = CodeBuf::new();
        let label = buf.bind_fresh_entry();
        let pc = buf.len();
        assert_eq!(buf.entry_label_for_offset(pc), Some(label));
        // Simulate discard_compile at a function entry (const-if cond).
        buf.push(Byte::new(Instruction::CONST).with_operand_u32(1));
        buf.truncate(pc);
        assert_eq!(
            buf.entry_label_for_offset(pc),
            Some(label),
            "entry at truncate point must survive for CALL→Entry rewrite"
        );
    }

    #[test]
    fn push_lifts_hot_set_bytes_to_typed_ops() {
        let mut buf = CodeBuf::new();
        buf.push(Byte::new(Instruction::LOAD).with_operand_u32(1));
        buf.push(Byte::new(Instruction::CONST).with_const_inline(2));
        buf.push(Byte::new(Instruction::ADD));
        buf.push(Byte::new(Instruction::RETURN));
        let ops = buf.ops();
        assert!(matches!(ops[0], IlOp::Load { slot: 1, .. }));
        assert!(matches!(ops[1], IlOp::Const { imm: 2, .. }));
        assert!(matches!(
            ops[2],
            IlOp::Bin {
                op: Instruction::ADD,
                ..
            }
        ));
        assert!(matches!(ops[3], IlOp::Return { .. }));
    }

    #[test]
    fn push_const_and_return_emit_typed_ops() {
        let mut buf = CodeBuf::new();
        buf.push_const(0);
        buf.push_return();
        let ops = buf.ops();
        assert!(matches!(ops[0], IlOp::Const { imm: 0, .. }));
        assert!(matches!(ops[1], IlOp::Return { .. }));
    }

    #[test]
    fn push_load_and_store_pop_emit_typed_ops() {
        let mut buf = CodeBuf::new();
        buf.push_load(3);
        buf.push_store_pop(4);
        let ops = buf.ops();
        assert!(matches!(ops[0], IlOp::Load { slot: 3, .. }));
        assert!(matches!(ops[1], IlOp::StorePop { slot: 4, .. }));
    }

    /// COI-19: `ffi_init` is spliced into the prologue via `splice_buf_at`.
    #[test]
    fn splice_buf_at_inserts_ops_before_code_pos() {
        let mut dest = CodeBuf::new();
        dest.push_const(1);
        dest.push_const(2);
        let mut src = CodeBuf::new();
        src.push_const(9);
        src.push_load(0);
        dest.splice_buf_at(1, src);
        let ops = dest.ops();
        assert_eq!(ops.len(), 4);
        assert!(matches!(ops[0], IlOp::Const { imm: 1, .. }));
        assert!(matches!(ops[1], IlOp::Const { imm: 9, .. }));
        assert!(matches!(ops[2], IlOp::Load { slot: 0, .. }));
        assert!(matches!(ops[3], IlOp::Const { imm: 2, .. }));
    }

    #[test]
    fn append_concatenates_typed_ops_and_remaps_labels() {
        let mut dest = CodeBuf::new();
        dest.push_const(1);
        let mut src = CodeBuf::new();
        let lab = src.il_mut().fresh_label();
        src.il_mut().emit_jump(IlJumpKind::Unconditional, lab);
        src.il_mut().bind_label(lab);
        src.push_const(2);
        dest.append(&mut src);
        assert!(src.is_empty());
        let ops = dest.ops();
        assert!(matches!(ops[0], IlOp::Const { imm: 1, .. }));
        assert!(matches!(ops[1], IlOp::Jump { .. }));
        assert!(matches!(ops[2], IlOp::Label(_)));
        assert!(matches!(ops[3], IlOp::Const { imm: 2, .. }));
        match (&ops[1], &ops[2]) {
            (IlOp::Jump { target, .. }, IlOp::Label(bound)) => assert_eq!(target, bound),
            _ => panic!("expected remapped jump/label"),
        }
    }

    #[test]
    fn record_func_tracks_spans_and_clear_drops_them() {
        let mut buf = CodeBuf::new();
        let entry = buf.bind_fresh_entry();
        buf.push(Byte::new(Instruction::ConstReturnImm).with_operand_u32(0));
        let end = buf.len();
        buf.record_func("main", Some(entry), 0, end);
        assert_eq!(buf.funcs().len(), 1);
        assert_eq!(buf.funcs()[0].name, "main");
        assert_eq!(buf.funcs()[0].entry, Some(entry));
        assert_eq!(buf.funcs()[0].code_start, 0);
        assert_eq!(buf.funcs()[0].code_end, end);
        buf.clear();
        assert!(buf.funcs().is_empty());
        assert!(buf.ops().is_empty());
    }

    /// Production finalize: owning `IlModule` + entry map must still resolve CALL.
    #[test]
    fn lower_in_place_resolves_entry_call_through_module() {
        let mut buf = CodeBuf::new();
        let entry = buf.bind_fresh_entry();
        let start = buf.len();
        buf.push_const(7);
        buf.push_return();
        let end = buf.len();
        buf.record_func("f", Some(entry), start, end);

        // Packed CALL to entry PC → Entry rewrite; lives in epilogue after split.
        buf.push(Byte::new(Instruction::CALL).with_call_packed(0, start as u32));
        buf.push(Byte::new(Instruction::HALT));

        let mut pool = Vec::new();
        let lowered = buf.lower_in_place(&mut pool);
        let ops: Vec<_> = lowered.bytecode.iter().map(|b| *b.bytecode()).collect();
        // Entry is a CALL target, not an uncond JMP join, so CONST; RETURN fuses.
        assert!(
            matches!(
                ops.as_slice(),
                [
                    Instruction::ConstReturnImm,
                    Instruction::CALL,
                    Instruction::HALT
                ]
            ),
            "unexpected lowered ops: {ops:?}"
        );
        assert_eq!(lowered.bytecode[0].operand_u32(), 7);
        assert_eq!(
            lowered.bytecode[1].call_parts(),
            (0, 0),
            "CALL must target entry PC after owning-module lower"
        );
        assert_eq!(buf.as_slice().len(), lowered.bytecode.len());
    }

    /// COI-108: reserve an entry label, emit `Entry{Call}` before the body, then
    /// bind the reserved label — lower must resolve CALL to the body PC.
    #[test]
    fn bind_reserved_entry_resolves_earlier_entry_call() {
        let mut buf = CodeBuf::new();
        let reserved = buf.fresh_label();
        // Call site ahead of the callee body (forward method reference).
        buf.emit_entry(EntryKind::Call, 0, reserved);
        buf.push(Byte::new(Instruction::HALT));

        let start = buf.len();
        buf.bind_reserved_entry(reserved);
        buf.push_const(3);
        buf.push_return();
        let end = buf.len();
        buf.record_func("later", Some(reserved), start, end);

        assert_eq!(
            buf.entry_label_for_offset(start),
            Some(reserved),
            "bind_reserved_entry must record entry_at_offset"
        );

        let mut pool = Vec::new();
        let lowered = buf.lower_in_place(&mut pool);
        let ops: Vec<_> = lowered.bytecode.iter().map(|b| *b.bytecode()).collect();
        assert!(
            ops.iter().any(|op| matches!(op, Instruction::CALL)),
            "reserved Entry{{Call}} must lower to CALL; ops={ops:?}"
        );
        let call = lowered
            .bytecode
            .iter()
            .find(|b| matches!(b.bytecode(), Instruction::CALL))
            .expect("CALL present");
        assert_eq!(
            call.call_parts(),
            (0, start),
            "CALL must target the reserved body PC"
        );
    }
}
