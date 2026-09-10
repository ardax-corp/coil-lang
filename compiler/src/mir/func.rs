//! MIR function / block containers and SSA verification.

use std::collections::HashMap;

use super::gc::LiveRootSet;
use super::inst::{BlockId, LocalId, MirInst, Terminator, ValueId};
use super::layout::MirLayout;
use super::ty::MirTy;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MirBlock {
    pub id: BlockId,
    pub insts: Vec<MirInst>,
    pub term: Option<Terminator>,
}

impl MirBlock {
    pub fn new(id: BlockId) -> Self {
        Self {
            id,
            insts: Vec::new(),
            term: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MirFunc {
    pub name: String,
    pub params: Vec<ValueId>,
    pub ret_ty: Option<MirTy>,
    pub ret_hi_ty: Option<MirTy>,
    pub ret_layout: MirLayout,
    pub entry: BlockId,
    pub blocks: Vec<MirBlock>,
    /// Type of each allocated value, indexed by [`ValueId::index`].
    pub types: Vec<MirTy>,
    /// IL slot → SSA value at each Alloc / GcBarrier dest (S2a).
    pub slot_env: HashMap<ValueId, Vec<(LocalId, ValueId)>>,
    /// Live heap words at each Alloc / GcBarrier (S2a).
    pub gc_roots: Vec<LiveRootSet>,
}

impl MirFunc {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            params: Vec::new(),
            ret_ty: None,
            ret_hi_ty: None,
            ret_layout: MirLayout::Word,
            entry: BlockId(0),
            blocks: vec![MirBlock::new(BlockId(0))],
            types: Vec::new(),
            slot_env: HashMap::new(),
            gc_roots: Vec::new(),
        }
    }

    /// Sidecar row for the Alloc or GcBarrier whose dest is `at`.
    pub fn live_roots_at(&self, at: ValueId) -> Option<&LiveRootSet> {
        self.gc_roots.iter().find(|s| s.at == at)
    }

    pub fn ty(&self, v: ValueId) -> MirTy {
        self.types.get(v.index()).copied().unwrap_or(MirTy::Bottom)
    }

    /// True when any HostInvoke is impure (I6).
    pub fn has_impure_host(&self) -> bool {
        self.blocks.iter().any(|b| {
            b.insts.iter().any(|i| match i {
                MirInst::HostInvoke { native_id, .. } => !super::effects::host_is_pure(*native_id),
                _ => false,
            })
        })
    }

    /// True when any inst is an alloc or GC placeholder (I5).
    pub fn has_gc_edge(&self) -> bool {
        self.blocks
            .iter()
            .any(|b| b.insts.iter().any(MirInst::is_gc_edge))
    }

    /// True when any inst is an explicit I7 deopt / stop edge.
    pub fn has_deopt_edge(&self) -> bool {
        self.blocks
            .iter()
            .any(|b| b.insts.iter().any(MirInst::is_deopt_edge))
    }

    pub fn block(&self, id: BlockId) -> &MirBlock {
        &self.blocks[id.index()]
    }

    pub fn block_mut(&mut self, id: BlockId) -> &mut MirBlock {
        &mut self.blocks[id.index()]
    }

    pub fn preds(&self) -> Vec<Vec<BlockId>> {
        let n = self.blocks.len();
        let mut preds = vec![Vec::new(); n];
        for b in &self.blocks {
            if let Some(term) = &b.term {
                for s in term.succs() {
                    if s.index() < n {
                        preds[s.index()].push(b.id);
                    }
                }
            }
        }
        preds
    }

    /// Structural SSA + type checks for the numeric subset.
    pub fn verify(&self) -> Result<(), String> {
        if self.blocks.is_empty() {
            return Err("function has no blocks".into());
        }
        if self.entry.index() >= self.blocks.len() {
            return Err("entry block out of range".into());
        }
        let mut defined = vec![false; self.types.len()];
        for &p in &self.params {
            if p.index() >= self.types.len() {
                return Err(format!("param {p} has no type"));
            }
            if !self.ty(p).is_specialized() {
                return Err(format!("param {p} is not a specialized SSA type"));
            }
            defined[p.index()] = true;
        }

        let preds = self.preds();
        let mut seen_dest = vec![false; self.types.len()];
        for p in &self.params {
            seen_dest[p.index()] = true;
        }

        for (bi, block) in self.blocks.iter().enumerate() {
            if block.id.index() != bi {
                return Err(format!("block id mismatch at {bi}"));
            }
            let Some(term) = &block.term else {
                return Err(format!("{} has no terminator", block.id));
            };
            let mut saw_non_phi = false;
            for inst in &block.insts {
                if inst.is_phi() {
                    if saw_non_phi {
                        return Err(format!("phi after non-phi in {}", block.id));
                    }
                } else {
                    saw_non_phi = true;
                }
                for dest in inst.dests() {
                    if dest.index() >= self.types.len() {
                        return Err(format!("dest {dest} has no type"));
                    }
                    if seen_dest[dest.index()] {
                        return Err(format!("value {dest} defined twice"));
                    }
                    seen_dest[dest.index()] = true;
                    defined[dest.index()] = true;
                }
                self.check_inst(inst, &preds[bi])?;
                for o in inst.operands() {
                    if o.index() >= self.types.len() || !defined[o.index()] && !inst.is_phi() {
                        // Phi operands may come from later blocks; checked vs preds.
                        if !inst.is_phi() {
                            return Err(format!("{} uses undefined {o}", inst.dest()));
                        }
                    }
                }
            }
            match term {
                Terminator::Br { cond, .. } => {
                    if self.ty(*cond) != MirTy::Bool {
                        return Err(format!("br cond {} is {}", cond, self.ty(*cond)));
                    }
                }
                Terminator::JumpIfMatch {
                    scrutinee,
                    tag,
                    payloads,
                    ..
                } => {
                    let _ = tag;
                    if payloads.len() > 1 {
                        return Err("JumpIfMatch arity > 1 (I2)".into());
                    }
                    if !self.ty(*scrutinee).is_specialized() {
                        return Err(format!(
                            "JumpIfMatch scrutinee {} is {}",
                            scrutinee,
                            self.ty(*scrutinee)
                        ));
                    }
                    for p in payloads {
                        if !self.ty(*p).is_specialized() {
                            return Err(format!("JumpIfMatch payload {p} is {}", self.ty(*p)));
                        }
                    }
                }
                Terminator::Return { lo, hi } => {
                    if hi.is_some() && lo.is_none() {
                        return Err("two-slot return missing payload word".into());
                    }
                    if let Some(v) = lo {
                        if let Some(rt) = self.ret_ty {
                            if !self.ty(*v).le(rt) && self.ty(*v) != rt {
                                return Err(format!("return {} : {} vs {rt}", v, self.ty(*v)));
                            }
                        }
                    }
                    if let Some(v) = hi {
                        if self.ret_layout != MirLayout::TwoSlot {
                            return Err("hi return word requires twoslot layout".into());
                        }
                        if let Some(rt) = self.ret_hi_ty {
                            if !self.ty(*v).le(rt) && self.ty(*v) != rt {
                                return Err(format!("return hi {} : {} vs {rt}", v, self.ty(*v)));
                            }
                        }
                    }
                }
                _ => {}
            }
            for s in term.succs() {
                if s.index() >= self.blocks.len() {
                    return Err(format!("{} jumps to missing {s}", block.id));
                }
            }
        }
        Ok(())
    }

    fn check_inst(&self, inst: &MirInst, preds: &[BlockId]) -> Result<(), String> {
        match inst {
            MirInst::Const { dest, c } => {
                if self.ty(*dest) != c.ty() {
                    return Err(format!("{dest} const type mismatch"));
                }
            }
            MirInst::Bin {
                dest,
                op,
                ty,
                lhs,
                rhs,
            } => {
                let lt = self.ty(*lhs);
                let rt = self.ty(*rhs);
                let heap_bit = ty.is_heap_word()
                    && matches!(
                        op,
                        super::inst::MirBinOp::BitAnd
                            | super::inst::MirBinOp::BitOr
                            | super::inst::MirBinOp::Xor
                    );
                if heap_bit {
                    let ok = |t: MirTy| t.is_heap_word() || t == MirTy::I64;
                    if !ok(lt) || !ok(rt) {
                        return Err(format!("{dest} heap bitwise operand type"));
                    }
                } else {
                    if !ty.is_numeric() || *ty == MirTy::Bool {
                        return Err(format!("{dest} binop on {ty}"));
                    }
                    if op.requires_int() && !ty.is_int() {
                        return Err(format!("{dest} bitwise on {ty}"));
                    }
                    if lt != *ty || rt != *ty {
                        return Err(format!("{dest} binop operand type"));
                    }
                }
                if self.ty(*dest) != *ty {
                    return Err(format!("{dest} binop dest type"));
                }
            }
            MirInst::Cmp {
                dest, ty, lhs, rhs, ..
            } => {
                let lt = self.ty(*lhs);
                let rt = self.ty(*rhs);
                if ty.is_heap_word() {
                    let ok = |t: MirTy| t.is_heap_word() || t == MirTy::I64;
                    if !ok(lt) || !ok(rt) {
                        return Err(format!("{dest} heap cmp operand type"));
                    }
                } else {
                    if !ty.is_numeric() || *ty == MirTy::Bool {
                        return Err(format!("{dest} cmp on {ty}"));
                    }
                    if lt != *ty || rt != *ty {
                        return Err(format!("{dest} cmp operand type"));
                    }
                }
                if self.ty(*dest) != MirTy::Bool {
                    return Err(format!("{dest} cmp dest is not bool"));
                }
            }
            MirInst::Unary { dest, op, src } => match op {
                super::inst::MirUnaryOp::Not => {
                    let t = self.ty(*src);
                    if self.ty(*dest) != MirTy::Bool {
                        return Err(format!("{dest} lnot dest is not bool"));
                    }
                    if t != MirTy::Bool && !t.is_int() && !t.is_heap_word() {
                        return Err(format!("{dest} lnot on {t}"));
                    }
                }
                super::inst::MirUnaryOp::Neg => {
                    let t = self.ty(*src);
                    if !t.is_int() && !t.is_float() {
                        return Err(format!("{dest} neg on {t}"));
                    }
                    if self.ty(*dest) != t {
                        return Err(format!("{dest} neg dest type"));
                    }
                }
            },
            MirInst::Cast {
                dest,
                kind,
                to,
                src,
            } => {
                let from = self.ty(*src);
                let ok = match kind {
                    super::inst::MirCastKind::IntToFloat => from.is_int() && to.is_float(),
                    super::inst::MirCastKind::Sext => from == MirTy::I32 && *to == MirTy::I64,
                };
                if !ok || self.ty(*dest) != *to {
                    return Err(format!("{dest} illegal cast {from} -> {to}"));
                }
            }
            MirInst::HostInvoke {
                dest,
                native_id,
                args,
            } => {
                let Some(spec) = super::host_allow::host_edge_spec(*native_id) else {
                    return Err(format!("{dest} host {native_id} not a typed edge"));
                };
                if spec.args.len() != args.len() {
                    return Err(format!("{dest} host arity"));
                }
                for (i, (a, ty)) in args.iter().zip(spec.args.iter()).enumerate() {
                    if self.ty(*a) != *ty {
                        return Err(format!("{dest} host arg {i} type"));
                    }
                }
                if self.ty(*dest) != spec.ret {
                    return Err(format!("{dest} host dest type"));
                }
            }
            MirInst::Call {
                dest,
                dest_hi,
                args,
                ..
            } => {
                if !self.ty(*dest).is_word_lane() {
                    return Err(format!("{dest} call dest is not a word lane"));
                }
                if let Some(hi) = dest_hi {
                    if !self.ty(*hi).is_word_lane() {
                        return Err(format!("{hi} call hi dest is not a word lane"));
                    }
                }
                for (i, a) in args.iter().enumerate() {
                    if !self.ty(*a).is_word_lane() {
                        return Err(format!("{dest} call arg {i} type"));
                    }
                }
            }
            MirInst::Index {
                dest,
                array,
                index,
                ..
            } => {
                if self.ty(*array) != MirTy::HeapRef {
                    return Err(format!("{dest} Index array is not heapref"));
                }
                if !self.ty(*index).is_int() {
                    return Err(format!("{dest} Index index type"));
                }
                if !self.ty(*dest).is_word_lane() {
                    return Err(format!("{dest} Index dest type"));
                }
            }
            MirInst::StoreIndex {
                dest,
                array,
                index,
                value,
                ..
            } => {
                if self.ty(*array) != MirTy::HeapRef {
                    return Err(format!("{dest} StoreIndex array is not heapref"));
                }
                if !self.ty(*index).is_int() {
                    return Err(format!("{dest} StoreIndex index type"));
                }
                if !self.ty(*value).is_word_lane() || self.ty(*dest) != self.ty(*value) {
                    return Err(format!("{dest} StoreIndex value type"));
                }
            }
            MirInst::ArrayLen { dest, array } => {
                if self.ty(*array) != MirTy::HeapRef {
                    return Err(format!("{dest} ArrayLen array is not heapref"));
                }
                if self.ty(*dest) != MirTy::I64 {
                    return Err(format!("{dest} ArrayLen dest type"));
                }
            }
            MirInst::MatchPayload {
                dest,
                scrutinee,
                index,
            } => {
                if *index > 0 {
                    return Err(format!("{dest} MatchPayload index"));
                }
                if !self.ty(*scrutinee).is_specialized() {
                    return Err(format!("{dest} MatchPayload scrutinee type"));
                }
                if !self.ty(*dest).is_specialized() {
                    return Err(format!("{dest} MatchPayload dest type"));
                }
            }
            MirInst::FieldLoad {
                dest,
                object,
                index,
                ..
            } => {
                if !self.ty(*object).is_specialized() {
                    return Err(format!("{dest} FieldLoad object type"));
                }
                if self.ty(*dest) != self.ty(*object) {
                    return Err(format!("{dest} FieldLoad dest type"));
                }
                if *index > 32 {
                    return Err(format!("{dest} FieldLoad index"));
                }
            }
            MirInst::FieldStore {
                dest, src, index, ..
            } => {
                if !self.ty(*src).is_specialized() {
                    return Err(format!("{dest} FieldStore src type"));
                }
                if self.ty(*dest) != self.ty(*src) {
                    return Err(format!("{dest} FieldStore dest type"));
                }
                if *index > 32 {
                    return Err(format!("{dest} FieldStore index"));
                }
            }
            MirInst::Alloc { dest, elems, .. } => {
                if self.ty(*dest) != MirTy::HeapRef {
                    return Err(format!("{dest} Alloc dest is not heapref"));
                }
                for (i, e) in elems.iter().enumerate() {
                    if !self.ty(*e).is_specialized() {
                        return Err(format!("{dest} Alloc elem {i} type"));
                    }
                }
            }
            MirInst::GcBarrier { dest, roots, .. } => {
                if self.ty(*dest) != MirTy::HeapRef {
                    return Err(format!("{dest} GcBarrier dest is not heapref"));
                }
                for (i, r) in roots.iter().enumerate() {
                    if !self.ty(*r).is_heap_word() {
                        return Err(format!("{dest} GcBarrier root {i} is not a heap word"));
                    }
                }
            }
            MirInst::Deopt { dest, .. } => {
                if self.ty(*dest) != MirTy::Bool {
                    return Err(format!("{dest} Deopt dest is not bool"));
                }
            }
            MirInst::String { dest, .. } => {
                if self.ty(*dest) != MirTy::HeapRef {
                    return Err(format!("{dest} String dest is not heapref"));
                }
            }
            MirInst::Print { dest, src } => {
                if self.ty(*dest) != MirTy::Bool {
                    return Err(format!("{dest} Print dest is not bool"));
                }
                if !self.ty(*src).is_specialized() {
                    return Err(format!("{dest} Print src type"));
                }
            }
            MirInst::Format { dest, fmt, args } => {
                if self.ty(*dest) != MirTy::HeapRef {
                    return Err(format!("{dest} Format dest is not heapref"));
                }
                if self.ty(*fmt) != MirTy::HeapRef && self.ty(*fmt) != MirTy::Value {
                    return Err(format!("{dest} Format fmt type"));
                }
                for (i, a) in args.iter().enumerate() {
                    if !self.ty(*a).is_specialized() {
                        return Err(format!("{dest} Format arg {i} type"));
                    }
                }
            }
            MirInst::Stringify { dest, src } => {
                if self.ty(*dest) != MirTy::HeapRef {
                    return Err(format!("{dest} Stringify dest is not heapref"));
                }
                if !self.ty(*src).is_specialized() {
                    return Err(format!("{dest} Stringify src type"));
                }
            }
            MirInst::Phi { dest, ty, args } => {
                if self.ty(*dest) != *ty {
                    return Err(format!("{dest} phi type"));
                }
                if !ty.is_specialized() {
                    return Err(format!("{dest} phi is not specialized"));
                }
                let mut seen = preds.to_vec();
                seen.sort();
                let mut got: Vec<BlockId> = args.iter().map(|(b, _)| *b).collect();
                got.sort();
                if got != seen {
                    return Err(format!("{dest} phi preds {got:?} != block preds {seen:?}"));
                }
                for (_, v) in args {
                    if self.ty(*v) != *ty {
                        return Err(format!("{dest} phi arg {v} type"));
                    }
                }
            }
        }
        Ok(())
    }
}
