//! MIR function / block containers and SSA verification.

use super::inst::{BlockId, MirInst, Terminator, ValueId};
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
        }
    }

    pub fn ty(&self, v: ValueId) -> MirTy {
        self.types.get(v.index()).copied().unwrap_or(MirTy::Bottom)
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
                return Err(format!("param {p} is not a specialized numeric type"));
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
                let dest = inst.dest();
                if dest.index() >= self.types.len() {
                    return Err(format!("dest {dest} has no type"));
                }
                if seen_dest[dest.index()] {
                    return Err(format!("value {dest} defined twice"));
                }
                seen_dest[dest.index()] = true;
                defined[dest.index()] = true;
                self.check_inst(inst, &preds[bi])?;
                for o in inst.operands() {
                    if o.index() >= self.types.len() || !defined[o.index()] && !inst.is_phi() {
                        // Phi operands may come from later blocks; checked vs preds.
                        if !inst.is_phi() {
                            return Err(format!("{dest} uses undefined {o}"));
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
                if !ty.is_specialized() || *ty == MirTy::Bool {
                    return Err(format!("{dest} binop on {ty}"));
                }
                if op.requires_int() && !ty.is_int() {
                    return Err(format!("{dest} bitwise on {ty}"));
                }
                if self.ty(*lhs) != *ty || self.ty(*rhs) != *ty {
                    return Err(format!("{dest} binop operand type"));
                }
                if self.ty(*dest) != *ty {
                    return Err(format!("{dest} binop dest type"));
                }
            }
            MirInst::Cmp {
                dest, ty, lhs, rhs, ..
            } => {
                if !ty.is_specialized() || *ty == MirTy::Bool {
                    return Err(format!("{dest} cmp on {ty}"));
                }
                if self.ty(*lhs) != *ty || self.ty(*rhs) != *ty {
                    return Err(format!("{dest} cmp operand type"));
                }
                if self.ty(*dest) != MirTy::Bool {
                    return Err(format!("{dest} cmp dest is not bool"));
                }
            }
            MirInst::Unary { dest, op, src } => match op {
                super::inst::MirUnaryOp::Not => {
                    if self.ty(*src) != MirTy::Bool || self.ty(*dest) != MirTy::Bool {
                        return Err(format!("{dest} bnot type"));
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
