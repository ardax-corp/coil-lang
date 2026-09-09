//! Braun-style SSA builder for the numeric subset.
//!
//! Locals are IL slots. Sealing a block completes incomplete φs. This is
//! construction only — fuse-IL stays the production lowering; dense exec is P1.

use std::collections::HashMap;

use super::func::{MirBlock, MirFunc};
use super::inst::{
    BlockId, LocalId, MirBinOp, MirCastKind, MirCmpOp, MirConst, MirInst, MirUnaryOp, Terminator,
    ValueId,
};
use super::ty::MirTy;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MirError {
    Msg(String),
}

impl std::fmt::Display for MirError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Msg(s) => f.write_str(s),
        }
    }
}

impl std::error::Error for MirError {}

impl MirError {
    fn msg(s: impl Into<String>) -> Self {
        Self::Msg(s.into())
    }
}

/// Incremental SSA constructor.
pub struct MirBuilder {
    func: MirFunc,
    current: Option<BlockId>,
    current_def: Vec<HashMap<LocalId, ValueId>>,
    sealed: Vec<bool>,
    preds: Vec<Vec<BlockId>>,
    incomplete_phis: Vec<Vec<(LocalId, ValueId)>>,
    subst: HashMap<ValueId, ValueId>,
    finished: bool,
    /// I6: type non-W4 HostInvoke as Value-word edges (barriers).
    pub allow_effects: bool,
    /// S2b: fill roots without SSA verify.
    pub skip_verify: bool,
}

impl MirBuilder {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            func: MirFunc::new(name),
            current: Some(BlockId(0)),
            current_def: vec![HashMap::new()],
            sealed: vec![false],
            preds: vec![Vec::new()],
            incomplete_phis: vec![Vec::new()],
            subst: HashMap::new(),
            finished: false,
            allow_effects: false,
            skip_verify: false,
        }
    }

    pub fn func(&self) -> &MirFunc {
        &self.func
    }

    pub fn entry(&self) -> BlockId {
        self.func.entry
    }

    #[allow(dead_code)]
    pub fn current_block(&self) -> Option<BlockId> {
        self.current
    }

    pub fn set_ret_ty(&mut self, ty: MirTy) {
        self.func.ret_ty = Some(ty);
    }

    pub fn add_param(&mut self, ty: MirTy) -> Result<ValueId, MirError> {
        if !ty.is_specialized() {
            return Err(MirError::msg("param must be a specialized SSA type"));
        }
        let v = self.alloc(ty);
        self.func.params.push(v);
        Ok(v)
    }

    /// Bind a source local to an SSA value in the current block.
    pub fn def_local(&mut self, local: LocalId, val: ValueId) -> Result<(), MirError> {
        let b = self.cur()?;
        self.write_variable(local, b, val);
        Ok(())
    }

    /// Read a source local, inserting φs at sealed / incomplete joins.
    pub fn use_local(&mut self, local: LocalId, ty: MirTy) -> Result<ValueId, MirError> {
        let b = self.cur()?;
        Ok(self.read_variable(local, ty, b))
    }

    pub fn create_block(&mut self) -> BlockId {
        let id = BlockId(self.func.blocks.len() as u32);
        self.func.blocks.push(MirBlock::new(id));
        self.current_def.push(HashMap::new());
        self.sealed.push(false);
        self.preds.push(Vec::new());
        self.incomplete_phis.push(Vec::new());
        id
    }

    pub fn switch_to_block(&mut self, b: BlockId) {
        self.current = Some(b);
    }

    pub fn seal_block(&mut self, b: BlockId) {
        if self.sealed[b.index()] {
            return;
        }
        self.sealed[b.index()] = true;
        let pending = std::mem::take(&mut self.incomplete_phis[b.index()]);
        for (local, phi) in pending {
            self.add_phi_operands(local, phi, b);
        }
    }

    pub fn seal_all(&mut self) {
        let n = self.func.blocks.len();
        for i in 0..n {
            self.seal_block(BlockId(i as u32));
        }
    }

    pub fn ins_const(&mut self, c: MirConst) -> Result<ValueId, MirError> {
        let dest = self.alloc(c.ty());
        self.push(MirInst::Const { dest, c })?;
        Ok(dest)
    }

    pub fn ins_binop(
        &mut self,
        op: MirBinOp,
        lhs: ValueId,
        rhs: ValueId,
    ) -> Result<ValueId, MirError> {
        let lt = self.resolve_ty(lhs);
        let rt = self.resolve_ty(rhs);
        let heap_bit = op.requires_int() && (lt.is_heap_word() || rt.is_heap_word());
        if heap_bit {
            if !matches!(op, MirBinOp::BitAnd | MirBinOp::BitOr | MirBinOp::Xor) {
                return Err(MirError::msg(format!("binop {op:?} on heap word")));
            }
            if !lt.is_heap_word() && lt != MirTy::I64 {
                return Err(MirError::msg(format!("binop operand types {lt} vs {rt}")));
            }
            if !rt.is_heap_word() && rt != MirTy::I64 {
                return Err(MirError::msg(format!("binop operand types {lt} vs {rt}")));
            }
        } else {
            if lt != rt {
                return Err(MirError::msg(format!("binop operand types {lt} vs {rt}")));
            }
            if !lt.is_numeric() || lt == MirTy::Bool {
                return Err(MirError::msg(format!("binop on {lt}")));
            }
            if op.requires_int() && !lt.is_int() {
                return Err(MirError::msg(format!("bitwise op on {lt}")));
            }
        }
        let dest_ty = if heap_bit {
            match op {
                MirBinOp::BitOr => MirTy::NicheRes,
                MirBinOp::BitAnd => MirTy::HeapRef,
                _ => {
                    if lt.is_heap_word() {
                        lt
                    } else {
                        rt
                    }
                }
            }
        } else {
            lt
        };
        let dest = self.alloc(dest_ty);
        self.push(MirInst::Bin {
            dest,
            op,
            ty: dest_ty,
            lhs: self.resolve(lhs),
            rhs: self.resolve(rhs),
        })?;
        Ok(dest)
    }

    pub fn ins_cmp(
        &mut self,
        op: MirCmpOp,
        lhs: ValueId,
        rhs: ValueId,
    ) -> Result<ValueId, MirError> {
        let lt = self.resolve_ty(lhs);
        let rt = self.resolve_ty(rhs);
        let heap_cmp =
            (lt.is_heap_word() || rt.is_heap_word()) && matches!(op, MirCmpOp::Eq | MirCmpOp::Ne);
        if heap_cmp {
            let ok = |t: MirTy| t.is_heap_word() || t == MirTy::I64;
            if !ok(lt) || !ok(rt) {
                return Err(MirError::msg(format!("cmp types {lt} vs {rt}")));
            }
        } else if lt != rt || !lt.is_numeric() || lt == MirTy::Bool {
            return Err(MirError::msg(format!("cmp types {lt} vs {rt}")));
        }
        let cmp_ty = if heap_cmp {
            if lt.is_heap_word() {
                lt
            } else {
                rt
            }
        } else {
            lt
        };
        let dest = self.alloc(MirTy::Bool);
        self.push(MirInst::Cmp {
            dest,
            op,
            ty: cmp_ty,
            lhs: self.resolve(lhs),
            rhs: self.resolve(rhs),
        })?;
        Ok(dest)
    }

    pub fn ins_neg(&mut self, src: ValueId) -> Result<ValueId, MirError> {
        let t = self.resolve_ty(src);
        if !t.is_int() && !t.is_float() {
            return Err(MirError::msg(format!("neg on {t}")));
        }
        let dest = self.alloc(t);
        self.push(MirInst::Unary {
            dest,
            op: MirUnaryOp::Neg,
            src: self.resolve(src),
        })?;
        Ok(dest)
    }

    pub fn ins_not(&mut self, src: ValueId) -> Result<ValueId, MirError> {
        let t = self.resolve_ty(src);
        // VM `LogNot` is truthiness: bool, i64, or a niche/heap word (`0` / ptr).
        if t != MirTy::Bool && !t.is_int() && !t.is_heap_word() {
            return Err(MirError::msg(format!("lnot on {t}")));
        }
        let dest = self.alloc(MirTy::Bool);
        self.push(MirInst::Unary {
            dest,
            op: MirUnaryOp::Not,
            src: self.resolve(src),
        })?;
        Ok(dest)
    }

    pub fn ins_cast(
        &mut self,
        kind: MirCastKind,
        to: MirTy,
        src: ValueId,
    ) -> Result<ValueId, MirError> {
        let from = self.resolve_ty(src);
        let ok = match kind {
            MirCastKind::IntToFloat => from.is_int() && to.is_float(),
            MirCastKind::Sext => from == MirTy::I32 && to == MirTy::I64,
        };
        if !ok {
            return Err(MirError::msg(format!("illegal cast {from} -> {to}")));
        }
        let dest = self.alloc(to);
        self.push(MirInst::Cast {
            dest,
            kind,
            to,
            src: self.resolve(src),
        })?;
        Ok(dest)
    }

    pub fn ins_host_invoke(
        &mut self,
        native_id: u16,
        args: Vec<ValueId>,
    ) -> Result<ValueId, MirError> {
        let spec = if self.allow_effects {
            super::host_allow::host_edge_spec(native_id)
        } else {
            super::host_allow::host_spec(native_id)
        }
        .ok_or_else(|| MirError::msg(format!("host {native_id} is not a typed MIR edge")))?;
        if spec.args.len() != args.len() {
            return Err(MirError::msg(format!(
                "host {} arity {} vs {}",
                spec.name,
                spec.args.len(),
                args.len()
            )));
        }
        let args: Vec<ValueId> = args.into_iter().map(|v| self.resolve(v)).collect();
        for (i, (&a, &ty)) in args.iter().zip(spec.args).enumerate() {
            if self.resolve_ty(a) != ty {
                return Err(MirError::msg(format!(
                    "host {} arg {i} is {} vs {ty}",
                    spec.name,
                    self.resolve_ty(a)
                )));
            }
        }
        let dest = self.alloc(spec.ret);
        self.push(MirInst::HostInvoke {
            dest,
            native_id,
            args,
        })?;
        Ok(dest)
    }

    pub fn ins_call(
        &mut self,
        target: crate::il::Label,
        args: Vec<ValueId>,
        abi: &super::abi::DenseAbi,
    ) -> Result<ValueId, MirError> {
        if abi.params.len() != args.len() {
            return Err(MirError::msg(format!(
                "call arity {} vs {}",
                abi.params.len(),
                args.len()
            )));
        }
        let args: Vec<ValueId> = args.into_iter().map(|v| self.resolve(v)).collect();
        if !abi.ret.is_word_lane() {
            return Err(MirError::msg(format!("call dest is {}", abi.ret)));
        }
        for (i, (&a, &ty)) in args.iter().zip(abi.params.iter()).enumerate() {
            if self.resolve_ty(a) != ty {
                return Err(MirError::msg(format!(
                    "call arg {i} is {} vs {ty}",
                    self.resolve_ty(a)
                )));
            }
            if !ty.is_word_lane() {
                return Err(MirError::msg(format!("call arg {i} is {ty}")));
            }
        }
        let dest = self.alloc(abi.ret);
        self.push(MirInst::Call { dest, target, args })?;
        Ok(dest)
    }

    /// Stack-join φ for values carried across CFG edges (I2 match diamonds).
    pub fn ins_stack_phi(&mut self, args: Vec<(BlockId, ValueId)>) -> Result<ValueId, MirError> {
        if args.is_empty() {
            return Err(MirError::msg("empty stack phi"));
        }
        let ty = self.resolve_ty(args[0].1);
        let dest = self.alloc(ty);
        let mut args: Vec<(BlockId, ValueId)> = args
            .into_iter()
            .map(|(b, v)| (b, self.resolve(v)))
            .collect();
        args.sort_by_key(|(b, _)| *b);
        self.push(MirInst::Phi { dest, ty, args })?;
        Ok(dest)
    }

    pub fn jump(&mut self, dest: BlockId) -> Result<(), MirError> {
        let src = self.cur()?;
        self.add_edge(src, dest);
        self.set_term(Terminator::Jump { dest })
    }

    pub fn jump_if_match(
        &mut self,
        scrutinee: ValueId,
        tag: u32,
        payloads: Vec<ValueId>,
        taken: BlockId,
        not_taken: BlockId,
    ) -> Result<(), MirError> {
        if payloads.len() > 1 {
            return Err(MirError::msg("I2 JumpIfMatch arity > 1"));
        }
        let st = self.resolve_ty(scrutinee);
        if !st.is_specialized() {
            return Err(MirError::msg(format!("JumpIfMatch scrutinee is {st}")));
        }
        let src = self.cur()?;
        self.add_edge(src, taken);
        self.add_edge(src, not_taken);
        self.set_term(Terminator::JumpIfMatch {
            scrutinee: self.resolve(scrutinee),
            tag,
            payloads: payloads.into_iter().map(|v| self.resolve(v)).collect(),
            taken,
            not_taken,
        })
    }

    pub fn ins_match_payload(
        &mut self,
        scrutinee: ValueId,
        index: u32,
        ty: MirTy,
    ) -> Result<ValueId, MirError> {
        if index > 0 {
            return Err(MirError::msg("I2 MatchPayload index > 0"));
        }
        if !ty.is_specialized() {
            return Err(MirError::msg(format!("MatchPayload type {ty}")));
        }
        let dest = self.alloc(ty);
        self.push(MirInst::MatchPayload {
            dest,
            scrutinee: self.resolve(scrutinee),
            index,
        })?;
        Ok(dest)
    }

    /// Identity copy of an unboxed field slot (I3). `object` is the current
    /// SSA value of `base + index`.
    pub fn ins_field_load(
        &mut self,
        object: ValueId,
        base: u32,
        index: u32,
    ) -> Result<ValueId, MirError> {
        let ty = self.resolve_ty(object);
        if !ty.is_specialized() {
            return Err(MirError::msg(format!("FieldLoad type {ty}")));
        }
        let dest = self.alloc(ty);
        self.push(MirInst::FieldLoad {
            dest,
            object: self.resolve(object),
            base,
            index,
        })?;
        Ok(dest)
    }

    /// Write `src` into unboxed field slot `base + index` (I3 ctor / rebind).
    pub fn ins_field_store(
        &mut self,
        src: ValueId,
        base: u32,
        index: u32,
    ) -> Result<ValueId, MirError> {
        let ty = self.resolve_ty(src);
        if !ty.is_specialized() {
            return Err(MirError::msg(format!("FieldStore type {ty}")));
        }
        let dest = self.alloc(ty);
        let src = self.resolve(src);
        self.def_local(LocalId(base + index), src)?;
        self.push(MirInst::FieldStore {
            dest,
            src,
            base,
            index,
        })?;
        Ok(dest)
    }

    /// Heap index load (S3). `array` is `heapref`; `index` is `i64`.
    pub fn ins_index(
        &mut self,
        array: ValueId,
        index: ValueId,
        dest_ty: MirTy,
        unchecked: bool,
    ) -> Result<ValueId, MirError> {
        let at = self.resolve_ty(array);
        if at != MirTy::HeapRef && at != MirTy::Value {
            return Err(MirError::msg(format!("Index array is {at}")));
        }
        let it = self.resolve_ty(index);
        if !it.is_int() {
            return Err(MirError::msg(format!("Index index is {it}")));
        }
        if !dest_ty.is_word_lane() {
            return Err(MirError::msg(format!("Index dest is {dest_ty}")));
        }
        let dest = self.alloc(dest_ty);
        self.push(MirInst::Index {
            dest,
            array: self.resolve(array),
            index: self.resolve(index),
            unchecked,
        })?;
        Ok(dest)
    }

    /// Heap index store (S3). Dest is the stored value.
    pub fn ins_store_index(
        &mut self,
        array: ValueId,
        index: ValueId,
        value: ValueId,
        unchecked: bool,
    ) -> Result<ValueId, MirError> {
        let at = self.resolve_ty(array);
        if at != MirTy::HeapRef && at != MirTy::Value {
            return Err(MirError::msg(format!("StoreIndex array is {at}")));
        }
        let it = self.resolve_ty(index);
        if !it.is_int() {
            return Err(MirError::msg(format!("StoreIndex index is {it}")));
        }
        let vt = self.resolve_ty(value);
        if !vt.is_word_lane() {
            return Err(MirError::msg(format!("StoreIndex value is {vt}")));
        }
        let dest = self.alloc(vt);
        self.push(MirInst::StoreIndex {
            dest,
            array: self.resolve(array),
            index: self.resolve(index),
            value: self.resolve(value),
            unchecked,
        })?;
        Ok(dest)
    }

    /// Structural `ArrayLen` (S3).
    pub fn ins_array_len(&mut self, array: ValueId) -> Result<ValueId, MirError> {
        let at = self.resolve_ty(array);
        if at != MirTy::HeapRef && at != MirTy::Value {
            return Err(MirError::msg(format!("ArrayLen array is {at}")));
        }
        let dest = self.alloc(MirTy::I64);
        self.push(MirInst::ArrayLen {
            dest,
            array: self.resolve(array),
        })?;
        Ok(dest)
    }

    /// Heap alloc (I5). Dest is `heapref`. Does not emit a barrier; call
    /// [`Self::ins_gc_barrier`] so the safepoint edge is visible.
    pub fn ins_alloc(
        &mut self,
        kind: super::inst::MirAllocKind,
        elems: Vec<ValueId>,
    ) -> Result<ValueId, MirError> {
        let elems: Vec<ValueId> = elems.into_iter().map(|v| self.resolve(v)).collect();
        for (i, &e) in elems.iter().enumerate() {
            if !self.resolve_ty(e).is_specialized() {
                return Err(MirError::msg(format!(
                    "Alloc elem {i} is {}",
                    self.resolve_ty(e)
                )));
            }
        }
        let dest = self.alloc(MirTy::HeapRef);
        self.push(MirInst::Alloc { dest, kind, elems })?;
        self.snapshot_slots(dest);
        Ok(dest)
    }

    /// GC safepoint (I5). Dest is a heapref token. `roots` is a seed;
    /// [`super::gc::fill_live_roots`] replaces it with live heap words.
    pub fn ins_gc_barrier(
        &mut self,
        kind: super::inst::MirGcKind,
        roots: Vec<ValueId>,
    ) -> Result<ValueId, MirError> {
        let roots: Vec<ValueId> = roots.into_iter().map(|v| self.resolve(v)).collect();
        for (i, &r) in roots.iter().enumerate() {
            if !self.resolve_ty(r).is_heap_word() {
                return Err(MirError::msg(format!(
                    "GcBarrier root {i} is {}",
                    self.resolve_ty(r)
                )));
            }
        }
        let dest = self.alloc(MirTy::HeapRef);
        self.push(MirInst::GcBarrier { dest, kind, roots })?;
        self.snapshot_slots(dest);
        Ok(dest)
    }

    /// Debugger stop / deopt placeholder (I7). Dest is a `bool` token.
    pub fn ins_deopt(
        &mut self,
        kind: super::inst::MirDeoptKind,
        loc: common::DebugLoc,
    ) -> Result<ValueId, MirError> {
        let dest = self.alloc(MirTy::Bool);
        self.push(MirInst::Deopt { dest, kind, loc })?;
        Ok(dest)
    }

    pub fn branch(
        &mut self,
        cond: ValueId,
        taken: BlockId,
        not_taken: BlockId,
    ) -> Result<(), MirError> {
        if self.resolve_ty(cond) != MirTy::Bool {
            return Err(MirError::msg("branch cond must be bool"));
        }
        let src = self.cur()?;
        self.add_edge(src, taken);
        self.add_edge(src, not_taken);
        self.set_term(Terminator::Br {
            cond: self.resolve(cond),
            taken,
            not_taken,
        })
    }

    pub fn ret(&mut self, value: Option<ValueId>) -> Result<(), MirError> {
        let lo = value.map(|v| self.resolve(v));
        if let Some(v) = lo {
            let ty = self.func.ret_ty.unwrap_or_else(|| self.resolve_ty(v));
            self.func.ret_ty = Some(ty);
            if ty.is_heap_word() {
                self.func.ret_layout = ty.layout();
            }
        }
        self.set_term(Terminator::Return { lo, hi: None })
    }

    pub fn ret_pair(&mut self, lo: ValueId, hi: ValueId) -> Result<(), MirError> {
        let lo = self.resolve(lo);
        let hi = self.resolve(hi);
        self.func.ret_ty = Some(self.func.ret_ty.unwrap_or_else(|| self.resolve_ty(lo)));
        self.func.ret_hi_ty = Some(self.func.ret_hi_ty.unwrap_or_else(|| self.resolve_ty(hi)));
        self.func.ret_layout = super::layout::MirLayout::TwoSlot;
        self.set_term(Terminator::Return {
            lo: Some(lo),
            hi: Some(hi),
        })
    }

    #[allow(dead_code)]
    pub fn unreachable(&mut self) -> Result<(), MirError> {
        self.set_term(Terminator::Unreachable)
    }

    pub fn finish(mut self) -> Result<MirFunc, MirError> {
        self.seal_all();
        self.rewrite_subst();
        self.finished = true;
        if self.func.has_gc_edge() {
            super::gc::fill_live_roots(&mut self.func);
        }
        if !self.skip_verify {
            self.func.verify().map_err(MirError::msg)?;
        }
        Ok(self.func)
    }

    fn snapshot_slots(&mut self, at: ValueId) {
        let Ok(b) = self.cur() else {
            return;
        };
        let mut env: Vec<(LocalId, ValueId)> = self.current_def[b.index()]
            .iter()
            .map(|(&l, &v)| (l, self.resolve(v)))
            .collect();
        env.sort_by_key(|(l, _)| l.0);
        self.func.slot_env.insert(at, env);
    }

    fn cur(&self) -> Result<BlockId, MirError> {
        self.current
            .ok_or_else(|| MirError::msg("no current block"))
    }

    fn alloc(&mut self, ty: MirTy) -> ValueId {
        let id = ValueId(self.func.types.len() as u32);
        self.func.types.push(ty);
        id
    }

    fn push(&mut self, inst: MirInst) -> Result<(), MirError> {
        let b = self.cur()?;
        let block = self.func.block_mut(b);
        if block.term.is_some() {
            return Err(MirError::msg(format!("{b} already terminated")));
        }
        if inst.is_phi() {
            let i = block
                .insts
                .iter()
                .position(|x| !x.is_phi())
                .unwrap_or(block.insts.len());
            block.insts.insert(i, inst);
        } else {
            block.insts.push(inst);
        }
        Ok(())
    }

    fn set_term(&mut self, term: Terminator) -> Result<(), MirError> {
        let b = self.cur()?;
        let block = self.func.block_mut(b);
        if block.term.is_some() {
            return Err(MirError::msg(format!("{b} already terminated")));
        }
        block.term = Some(term);
        Ok(())
    }

    fn add_edge(&mut self, src: BlockId, dest: BlockId) {
        let preds = &mut self.preds[dest.index()];
        if !preds.contains(&src) {
            preds.push(src);
        }
    }

    fn write_variable(&mut self, local: LocalId, block: BlockId, val: ValueId) {
        self.current_def[block.index()].insert(local, val);
    }

    fn read_variable(&mut self, local: LocalId, ty: MirTy, block: BlockId) -> ValueId {
        if let Some(&v) = self.current_def[block.index()].get(&local) {
            return self.resolve(v);
        }
        self.read_variable_recursive(local, ty, block)
    }

    fn read_variable_recursive(&mut self, local: LocalId, ty: MirTy, block: BlockId) -> ValueId {
        if !self.sealed[block.index()] {
            let phi = self.make_phi(block, ty);
            self.incomplete_phis[block.index()].push((local, phi));
            self.write_variable(local, block, phi);
            return phi;
        }
        match self.preds[block.index()].len() {
            0 => {
                // Live-in: treat as a parameter of this fragment.
                let v = self.alloc(ty);
                if !self.func.params.contains(&v) {
                    self.func.params.push(v);
                }
                self.write_variable(local, block, v);
                v
            }
            1 => {
                let pred = self.preds[block.index()][0];
                let v = self.read_variable(local, ty, pred);
                self.write_variable(local, block, v);
                v
            }
            _ => {
                let phi = self.make_phi(block, ty);
                self.write_variable(local, block, phi);
                self.add_phi_operands(local, phi, block);
                self.resolve(phi)
            }
        }
    }

    fn make_phi(&mut self, block: BlockId, ty: MirTy) -> ValueId {
        let dest = self.alloc(ty);
        let inst = MirInst::Phi {
            dest,
            ty,
            args: Vec::new(),
        };
        let b = self.func.block_mut(block);
        let i = b
            .insts
            .iter()
            .position(|x| !x.is_phi())
            .unwrap_or(b.insts.len());
        b.insts.insert(i, inst);
        dest
    }

    fn add_phi_operands(&mut self, local: LocalId, phi: ValueId, block: BlockId) {
        let ty = self.func.ty(phi);
        let preds = self.preds[block.index()].clone();
        let mut args = Vec::new();
        for pred in preds {
            let v = self.read_variable(local, ty, pred);
            args.push((pred, self.resolve(v)));
        }
        args.sort_by_key(|(b, _)| *b);
        if let Some(inst) = self
            .func
            .block_mut(block)
            .insts
            .iter_mut()
            .find(|i| i.dest() == phi)
        {
            if let MirInst::Phi { args: slot, .. } = inst {
                *slot = args;
            }
        }
        self.try_remove_trivial_phi(phi, block);
    }

    fn try_remove_trivial_phi(&mut self, phi: ValueId, block: BlockId) {
        let Some(MirInst::Phi { args, .. }) = self
            .func
            .block(block)
            .insts
            .iter()
            .find(|i| i.dest() == phi)
            .cloned()
        else {
            return;
        };
        let mut same: Option<ValueId> = None;
        for (_, v) in args {
            let v = self.resolve(v);
            if v == phi {
                continue;
            }
            if let Some(s) = same {
                if s != v {
                    return;
                }
            } else {
                same = Some(v);
            }
        }
        let Some(same) = same else {
            return;
        };
        self.subst.insert(phi, same);
        self.func.block_mut(block).insts.retain(|i| i.dest() != phi);
    }

    fn resolve(&self, mut v: ValueId) -> ValueId {
        while let Some(&n) = self.subst.get(&v) {
            if n == v {
                break;
            }
            v = n;
        }
        v
    }

    fn resolve_ty(&self, v: ValueId) -> MirTy {
        self.func.ty(self.resolve(v))
    }

    fn rewrite_subst(&mut self) {
        if self.subst.is_empty() {
            return;
        }
        let subst = self.subst.clone();
        let map = |v: ValueId| {
            let mut cur = v;
            while let Some(&n) = subst.get(&cur) {
                if n == cur {
                    break;
                }
                cur = n;
            }
            cur
        };
        for block in &mut self.func.blocks {
            for inst in &mut block.insts {
                inst.rewrite_values(map);
            }
            if let Some(term) = &mut block.term {
                term.rewrite_values(map);
            }
        }
        for p in &mut self.func.params {
            *p = map(*p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_smoke_diamond() {
        let mut b = MirBuilder::new("diamond");
        let x = b.add_param(MirTy::I64).unwrap();
        b.def_local(LocalId(0), x).unwrap();
        let entry = b.entry();
        let then_b = b.create_block();
        let else_b = b.create_block();
        let join = b.create_block();

        b.switch_to_block(entry);
        let zero = b.ins_const(MirConst::I64(0)).unwrap();
        let c = b.ins_cmp(MirCmpOp::Lt, x, zero).unwrap();
        b.branch(c, then_b, else_b).unwrap();

        b.switch_to_block(then_b);
        let one = b.ins_const(MirConst::I64(1)).unwrap();
        b.def_local(LocalId(0), one).unwrap();
        b.jump(join).unwrap();

        b.switch_to_block(else_b);
        let two = b.ins_const(MirConst::I64(2)).unwrap();
        b.def_local(LocalId(0), two).unwrap();
        b.jump(join).unwrap();

        b.switch_to_block(join);
        let y = b.use_local(LocalId(0), MirTy::I64).unwrap();
        b.ret(Some(y)).unwrap();

        let f = b.finish().unwrap();
        assert!(
            f.block(join)
                .insts
                .iter()
                .any(|i| matches!(i, MirInst::Phi { .. })),
            "join must have a phi: {f:?}"
        );
        f.verify().unwrap();
    }

    #[test]
    fn refuses_class_shaped_value_param() {
        let mut b = MirBuilder::new("no_class");
        assert!(b.add_param(MirTy::Value).is_err());
    }

    #[test]
    fn accepts_heapref_and_niche_params() {
        for ty in [MirTy::HeapRef, MirTy::NicheOpt, MirTy::NicheRes] {
            let mut b = MirBuilder::new("href");
            let p = b.add_param(ty).unwrap();
            b.ret(Some(p)).unwrap();
            let f = b.finish().unwrap();
            assert_eq!(f.ty(p), ty);
            assert_eq!(f.ret_ty, Some(ty));
            assert_eq!(f.ret_layout, ty.layout());
        }
    }

    #[test]
    fn alloc_then_safepoint_is_heapref() {
        use crate::mir::inst::{MirAllocKind, MirGcKind};
        let mut b = MirBuilder::new("mk");
        let n = b.ins_const(MirConst::I64(1)).unwrap();
        let a = b.ins_alloc(MirAllocKind::Array, vec![n]).unwrap();
        let g = b.ins_gc_barrier(MirGcKind::Safepoint, vec![a]).unwrap();
        b.ret(Some(g)).unwrap();
        let f = b.finish().unwrap();
        assert_eq!(f.ty(a), MirTy::HeapRef);
        assert_eq!(f.ty(g), MirTy::HeapRef);
        assert!(f.blocks.iter().any(|bl| {
            bl.insts
                .iter()
                .any(|i| matches!(i, MirInst::Alloc { .. }) && i.is_gc_edge())
        }));
        assert!(f.has_gc_edge());
    }
}
