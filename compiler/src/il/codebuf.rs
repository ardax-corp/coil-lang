//! Bytecode buffer backed by stack IL during emit; lowered at finalize.

use std::collections::HashMap;

use common::{Byte, DebugLoc, Instruction};

use super::{EntryKind, IlBuilder, IlFunc, IlOp, Label};

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

    pub fn push_return(&mut self) {
        self.invalidate_lowered();
        self.il.push_return();
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
        self.rewrite_abs_entries_in(from_code, usize::MAX);
    }

    fn rewrite_abs_entries_in(&mut self, from_code: usize, to_code: usize) {
        let mut rewrites = Vec::new();
        let range = self.il.raw_range_of_code(from_code, to_code);
        for (i, op) in self.il.ops()[range.clone()].iter().enumerate() {
            if let IlOp::Byte { byte, loc } = op
                && let Some((kind, arity, label, ret_words)) = self.entry_from_abs_byte(*byte)
            {
                rewrites.push((range.start + i, kind, arity, label, *loc, ret_words));
            }
        }
        // A byte and its `Entry` both emit code: the code index stays valid.
        for (i, kind, arity, target, loc, ret_words) in rewrites {
            self.il.ops_slice_mut()[i] = IlOp::Entry {
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

    /// Attach the MIR lowered from HIR to the last recorded function.
    pub fn set_last_func_hir_mir(&mut self, func: crate::mir::MirFunc) {
        if let Some(f) = self.funcs.last_mut() {
            f.hir_mir = Some(Box::new(func));
        }
    }

    /// Record on the last recorded function why it did not lower straight
    /// from HIR to MIR.
    pub fn set_last_func_hir_mir_why(&mut self, why: String) {
        if let Some(f) = self.funcs.last_mut() {
            f.hir_mir_why = Some(why);
        }
    }

    /// Mark the last recorded function [`IlFunc::pinned`].
    pub fn set_last_func_pinned(&mut self) {
        if let Some(f) = self.funcs.last_mut() {
            f.pinned = true;
        }
    }

    pub fn funcs(&self) -> &[IlFunc] {
        &self.funcs
    }

    /// Keep only function records matching `pred` (treeshake after deletes).
    pub fn retain_funcs(&mut self, mut pred: impl FnMut(&IlFunc) -> bool) {
        self.funcs.retain(|f| pred(f));
    }

    pub fn fresh_label(&mut self) -> Label {
        self.il.fresh_label()
    }

    pub fn bind_label(&mut self, label: Label) {
        self.il.bind_label(label);
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

    pub fn push_prologue_jmp(&mut self) {
        self.il.push_prologue_jmp();
    }

    pub fn as_slice(&self) -> &[Byte] {
        self.lowered.as_deref().unwrap_or(&[])
    }

    /// The lowered bytes, for in-place patches that keep their length.
    pub fn lowered_mut(&mut self) -> Option<&mut [Byte]> {
        self.lowered.as_deref_mut()
    }

    pub fn clone_bytes(&self) -> Vec<Byte> {
        self.lowered.clone().unwrap_or_default()
    }

    pub fn ops(&self) -> &[IlOp] {
        self.il.ops()
    }

}

/// Disjoint deleted emitting ranges, ascending, and how far each PC after
/// them moves down.
#[derive(Debug, Default)]
pub struct DeleteShift {
    spans: Vec<(usize, usize)>,
    /// `before[i]`: the length of `spans[..i]`.
    before: Vec<usize>,
}

impl DeleteShift {

}

// Test fixtures build their IL with these.
#[cfg(test)]
impl CodeBuf {
    pub fn push_store_pop(&mut self, slot: u32) {
        self.invalidate_lowered();
        self.il.push_store_pop(slot);
    }

    pub fn emit_entry(&mut self, kind: EntryKind, arity: u32, target: Label) {
        self.il.emit_entry(kind, arity, target);
    }

    pub fn push_const(&mut self, imm: i32) {
        self.invalidate_lowered();
        self.il.push_const(imm);
    }

    pub fn push_load(&mut self, slot: u32) {
        self.invalidate_lowered();
        self.il.push_load(slot);
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use common::Instruction;

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

}

