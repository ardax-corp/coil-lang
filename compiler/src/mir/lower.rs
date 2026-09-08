//! Lower pre-fuse stack IL into numeric SSA.
//!
//! Fuse-select remains the production bytecode lowerer. This path is an
//! optional sidecar: classes, heap, non-dense user calls, and residual `Byte` (except a
//! small numeric set) refuse so the existing `Value` interpreter is unchanged.

use std::collections::{BTreeSet, HashMap};

use common::Instruction;

use crate::il::{EntryKind, IlJumpKind, IlOp, Label};

use super::abi::DenseCallMap;
use super::builder::{MirBuilder, MirError};
use super::func::MirFunc;
use super::gc::is_alloc_inst;
use super::inst::{
    BlockId, LocalId, MirAllocKind, MirBinOp, MirCastKind, MirCmpOp, MirConst, MirGcKind, MirInst,
    ValueId,
};
use super::string_barrier::{is_format_inst, refuse_reason};
use super::ty::MirTy;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LowerError {
    Refused(String),
    Mir(MirError),
}

impl std::fmt::Display for LowerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(s) => write!(f, "numeric MIR refused: {s}"),
            Self::Mir(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for LowerError {}

impl From<MirError> for LowerError {
    fn from(e: MirError) -> Self {
        Self::Mir(e)
    }
}

/// Slot / const-pool typing for a numeric fragment.
#[derive(Clone, Debug)]
pub struct LowerHints {
    pub name: String,
    pub slot_ty: HashMap<u32, MirTy>,
    pub default_int: MirTy,
    pub default_float: MirTy,
    pub pool: Vec<u64>,
    pub pool_ty: Vec<Option<MirTy>>,
    /// CALL-edge arity: slots `0..param_count` are live-in params (Value ABI).
    pub param_count: u32,
    /// Leaf-first dense callees (COI-291). Empty → user `CALL` still refuses.
    pub calls: DenseCallMap,
    /// I2: `JumpIfMatch` / `Unpack` / `Seek` and stack-carrying CFG edges.
    pub allow_match: bool,
    /// I3: unboxed class field ranges `(base, n)` from local_escape codegen.
    pub unboxed_fields: Vec<(u32, u32)>,
    /// I3: Load/Store of those slots become FieldLoad/FieldStore.
    pub allow_fields: bool,
    /// I5: `MakeArray` / `MakeTuple` / `MakeEnum` / `InitTyped` → Alloc +
    /// GcBarrier. Dense / LIR emit still refuse.
    pub allow_alloc: bool,
}

impl Default for LowerHints {
    fn default() -> Self {
        Self {
            name: "numeric".into(),
            slot_ty: HashMap::new(),
            default_int: MirTy::I64,
            default_float: MirTy::F64,
            pool: Vec::new(),
            pool_ty: Vec::new(),
            param_count: 0,
            calls: DenseCallMap::new(),
            allow_match: false,
            unboxed_fields: Vec::new(),
            allow_fields: false,
            allow_alloc: false,
        }
    }
}

impl LowerHints {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }

    fn slot(&self, slot: u32) -> MirTy {
        self.slot_ty.get(&slot).copied().unwrap_or(self.default_int)
    }

    fn pool_const(&self, idx: u32) -> Result<MirConst, LowerError> {
        let bits = *self
            .pool
            .get(idx as usize)
            .ok_or_else(|| LowerError::Refused(format!("const pool {idx}")))?;
        let ty = self
            .pool_ty
            .get(idx as usize)
            .copied()
            .flatten()
            .unwrap_or(self.default_int);
        Ok(match ty {
            MirTy::I32 => MirConst::I32(bits as i32),
            MirTy::I64 => MirConst::I64(bits as i64),
            MirTy::F32 => MirConst::F32(bits as u32),
            MirTy::F64 => MirConst::F64(bits),
            MirTy::Bool => MirConst::Bool(bits != 0),
            other => {
                return Err(LowerError::Refused(format!(
                    "const pool {idx} has type {other}"
                )));
            }
        })
    }

    fn field_of(&self, slot: u32) -> Option<(u32, u32)> {
        if !self.allow_fields {
            return None;
        }
        self.unboxed_fields.iter().find_map(|&(base, n)| {
            if slot >= base && slot < base + n {
                Some((base, slot - base))
            } else {
                None
            }
        })
    }
}

/// Lower a pre-fuse IL fragment that stays inside the numeric subset.
pub fn try_lower_numeric(ops: &[IlOp], hints: &LowerHints) -> Result<MirFunc, LowerError> {
    if ops.is_empty() {
        return Err(LowerError::Refused("empty IL".into()));
    }
    let ranges = split_blocks(ops);
    let mut label_block: HashMap<Label, BlockId> = HashMap::new();
    let mut b = MirBuilder::new(hints.name.clone());
    for i in 0..hints.param_count {
        let ty = hints.slot(i);
        if !ty.is_specialized() {
            return Err(LowerError::Refused(format!("param slot {i} is {ty}")));
        }
        let v = b.add_param(ty)?;
        b.def_local(LocalId(i), v)?;
    }
    let mut range_blocks: Vec<BlockId> = Vec::with_capacity(ranges.len());
    for (i, (start, _)) in ranges.iter().enumerate() {
        let bid = if i == 0 { b.entry() } else { b.create_block() };
        range_blocks.push(bid);
        if let Some(l) = label_at(&ops[*start]) {
            label_block.insert(l, bid);
        }
    }
    // Labels that begin a range after a non-label leader still bind.
    for (i, (start, _)) in ranges.iter().enumerate() {
        for op in &ops[*start..*start + 1] {
            if let Some(l) = label_at(op) {
                label_block.insert(l, range_blocks[i]);
            }
        }
        for op in &ops[ranges[i].0..ranges[i].1] {
            if let Some(l) = label_at(op) {
                label_block.insert(l, range_blocks[i]);
            }
        }
    }

    let mut incoming: HashMap<BlockId, Vec<(BlockId, Vec<ValueId>)>> = HashMap::new();
    let mut started: HashMap<BlockId, Vec<ValueId>> = HashMap::new();
    for (i, &(start, end)) in ranges.iter().enumerate() {
        let bid = range_blocks[i];
        b.switch_to_block(bid);
        let mut tos = merge_incoming(&mut b, incoming.remove(&bid).unwrap_or_default())?;
        if let Some(prev) = started.get(&bid) {
            if prev != &tos {
                return Err(LowerError::Refused("back-edge stack mismatch (I2)".into()));
            }
        } else {
            started.insert(bid, tos.clone());
        }
        for op in &ops[start..end] {
            lower_op(&mut b, &mut tos, op, hints)?;
        }
        if b.func().block(bid).term.is_none() {
            let last = ops.get(end.saturating_sub(1));
            emit_term(
                &mut b,
                &mut tos,
                last,
                &label_block,
                ranges.get(i + 1).map(|_| range_blocks[i + 1]),
                hints,
                bid,
                &mut incoming,
            )?;
        }
        if !tos.is_empty()
            && !hints.allow_match
            && b.func()
                .block(bid)
                .term
                .as_ref()
                .is_some_and(|t| !matches!(t, crate::mir::inst::Terminator::Return { .. }))
        {
            return Err(LowerError::Refused(
                "non-empty operand stack at CFG edge (P0)".into(),
            ));
        }
    }
    for (succ, stacks) in incoming {
        let Some(used) = started.get(&succ) else {
            continue;
        };
        if stacks.iter().any(|(_, s)| s != used) {
            return Err(LowerError::Refused("back-edge stack mismatch (I2)".into()));
        }
    }
    b.finish().map_err(LowerError::from)
}

fn merge_incoming(
    b: &mut MirBuilder,
    preds: Vec<(BlockId, Vec<ValueId>)>,
) -> Result<Vec<ValueId>, LowerError> {
    if preds.is_empty() {
        return Ok(Vec::new());
    }
    let h = preds[0].1.len();
    if preds.iter().any(|(_, s)| s.len() != h) {
        return Err(LowerError::Refused("edge stack height mismatch".into()));
    }
    let mut tos = Vec::with_capacity(h);
    for i in 0..h {
        let v0 = preds[0].1[i];
        if preds.iter().all(|(_, s)| s[i] == v0) {
            tos.push(v0);
            continue;
        }
        let args: Vec<(BlockId, ValueId)> = preds.iter().map(|(p, s)| (*p, s[i])).collect();
        tos.push(b.ins_stack_phi(args)?);
    }
    Ok(tos)
}

fn record_edge(
    incoming: &mut HashMap<BlockId, Vec<(BlockId, Vec<ValueId>)>>,
    pred: BlockId,
    succ: BlockId,
    stack: Vec<ValueId>,
) {
    incoming.entry(succ).or_default().push((pred, stack));
}

fn label_at(op: &IlOp) -> Option<Label> {
    match *op {
        IlOp::Label(l) | IlOp::JoinLabel(l) => Some(l),
        _ => None,
    }
}

fn split_blocks(ops: &[IlOp]) -> Vec<(usize, usize)> {
    let n = ops.len();
    let mut leaders = BTreeSet::new();
    leaders.insert(0);
    for (i, op) in ops.iter().enumerate() {
        if label_at(op).is_some() {
            leaders.insert(i);
        }
        if is_term(op) && i + 1 < n {
            leaders.insert(i + 1);
        }
    }
    let marks: Vec<usize> = leaders.into_iter().collect();
    let mut ranges = Vec::new();
    for (k, &start) in marks.iter().enumerate() {
        let end = marks.get(k + 1).copied().unwrap_or(n);
        if start < end {
            ranges.push((start, end));
        }
    }
    ranges
}

fn is_term(op: &IlOp) -> bool {
    match op {
        IlOp::Entry {
            kind: EntryKind::Call,
            ..
        } => false,
        IlOp::Jump { .. }
        | IlOp::Return { .. }
        | IlOp::Halt { .. }
        | IlOp::Entry { .. }
        | IlOp::LoadReturnSlot { .. }
        | IlOp::ConstReturnImm { .. }
        | IlOp::BinReturn { .. }
        | IlOp::PrologueJmp { .. } => true,
        _ => false,
    }
}

fn emit_term(
    b: &mut MirBuilder,
    tos: &mut Vec<ValueId>,
    last: Option<&IlOp>,
    labels: &HashMap<Label, BlockId>,
    fallthrough: Option<BlockId>,
    hints: &LowerHints,
    pred: BlockId,
    incoming: &mut HashMap<BlockId, Vec<(BlockId, Vec<ValueId>)>>,
) -> Result<(), LowerError> {
    match last {
        Some(IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target,
            ..
        }) => {
            let dest = *labels
                .get(target)
                .ok_or_else(|| LowerError::Refused(format!("unbound {target:?}")))?;
            record_edge(incoming, pred, dest, tos.clone());
            b.jump(dest)?;
        }
        Some(IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            target,
            ..
        }) => {
            let cond = tos
                .pop()
                .ok_or_else(|| LowerError::Refused("jmpf stack".into()))?;
            let not_taken = *labels
                .get(target)
                .ok_or_else(|| LowerError::Refused(format!("unbound {target:?}")))?;
            let taken =
                fallthrough.ok_or_else(|| LowerError::Refused("jmpf fallthrough".into()))?;
            record_edge(incoming, pred, taken, tos.clone());
            record_edge(incoming, pred, not_taken, tos.clone());
            b.branch(cond, taken, not_taken)?;
        }
        Some(IlOp::Jump {
            kind: IlJumpKind::JumpIfTrue,
            target,
            ..
        }) => {
            let cond = tos
                .pop()
                .ok_or_else(|| LowerError::Refused("jmpt stack".into()))?;
            let taken = *labels
                .get(target)
                .ok_or_else(|| LowerError::Refused(format!("unbound {target:?}")))?;
            let not_taken =
                fallthrough.ok_or_else(|| LowerError::Refused("jmpt fallthrough".into()))?;
            record_edge(incoming, pred, taken, tos.clone());
            record_edge(incoming, pred, not_taken, tos.clone());
            b.branch(cond, taken, not_taken)?;
        }
        Some(IlOp::Return { ret_words, .. }) if *ret_words >= 2 => {
            let hi = tos
                .pop()
                .ok_or_else(|| LowerError::Refused("ret2 tag stack".into()))?;
            let lo = tos
                .pop()
                .ok_or_else(|| LowerError::Refused("ret2 payload stack".into()))?;
            b.ret_pair(lo, hi)?;
        }
        Some(IlOp::Return { .. }) | Some(IlOp::Halt { .. }) => {
            b.ret(tos.pop())?;
        }
        Some(IlOp::Jump {
            kind: IlJumpKind::JumpIfMatch { tag, arity },
            target,
            ..
        }) => {
            if !hints.allow_match {
                return Err(LowerError::Refused("JumpIfMatch / classes".into()));
            }
            if *tag > 1 || *arity > 1 {
                return Err(LowerError::Refused(
                    "JumpIfMatch full enum (I2 is niche/two-slot)".into(),
                ));
            }
            let scrutinee = tos
                .pop()
                .ok_or_else(|| LowerError::Refused("JumpIfMatch stack".into()))?;
            let taken = *labels
                .get(target)
                .ok_or_else(|| LowerError::Refused(format!("unbound {target:?}")))?;
            let not_taken =
                fallthrough.ok_or_else(|| LowerError::Refused("JumpIfMatch fallthrough".into()))?;
            let mut payloads = Vec::new();
            if *arity == 1 {
                let ty = match b.func().ty(scrutinee) {
                    MirTy::NicheOpt | MirTy::NicheRes | MirTy::HeapRef => MirTy::HeapRef,
                    other => other,
                };
                payloads.push(b.ins_match_payload(scrutinee, 0, ty)?);
            }
            let mut taken_stack = tos.clone();
            taken_stack.extend(payloads.iter().copied());
            let mut miss_stack = tos.clone();
            miss_stack.push(scrutinee);
            record_edge(incoming, pred, taken, taken_stack);
            record_edge(incoming, pred, not_taken, miss_stack);
            b.jump_if_match(scrutinee, *tag, payloads, taken, not_taken)?;
            tos.clear();
        }
        Some(other) if is_term(other) => {
            return Err(LowerError::Refused("unsupported IL terminator".into()));
        }
        _ => {
            if let Some(ft) = fallthrough {
                record_edge(incoming, pred, ft, tos.clone());
                b.jump(ft)?;
            } else {
                b.ret(tos.pop())?;
            }
        }
    }
    Ok(())
}

fn lower_op(
    b: &mut MirBuilder,
    tos: &mut Vec<ValueId>,
    op: &IlOp,
    hints: &LowerHints,
) -> Result<(), LowerError> {
    match op {
        IlOp::Label(_) | IlOp::JoinLabel(_) => Ok(()),
        IlOp::Load { slot, .. } => {
            let ty = hints.slot(*slot);
            let v = b.use_local(LocalId(*slot), ty)?;
            if let Some((base, index)) = hints.field_of(*slot) {
                tos.push(b.ins_field_load(v, base, index)?);
            } else {
                tos.push(v);
            }
            Ok(())
        }
        IlOp::StorePop { slot, .. } => {
            let v = tos
                .pop()
                .ok_or_else(|| LowerError::Refused("store stack".into()))?;
            if let Some((base, index)) = hints.field_of(*slot) {
                let _ = b.ins_field_store(v, base, index)?;
            } else {
                b.def_local(LocalId(*slot), v)?;
            }
            Ok(())
        }
        IlOp::Const { imm, .. } => {
            let c = match hints.default_int {
                MirTy::I32 => MirConst::I32(*imm),
                _ => MirConst::I64(i64::from(*imm)),
            };
            tos.push(b.ins_const(c)?);
            Ok(())
        }
        IlOp::ConstPool { idx, .. } => {
            tos.push(b.ins_const(hints.pool_const(*idx)?)?);
            Ok(())
        }
        IlOp::Dup { .. } => {
            let v = *tos
                .last()
                .ok_or_else(|| LowerError::Refused("dup stack".into()))?;
            tos.push(v);
            Ok(())
        }
        IlOp::Pop { .. } => {
            tos.pop()
                .ok_or_else(|| LowerError::Refused("pop stack".into()))?;
            Ok(())
        }
        IlOp::LogNot { .. } => {
            let v = tos
                .pop()
                .ok_or_else(|| LowerError::Refused("not stack".into()))?;
            let t = b.func().ty(v);
            // Dense keeps bool-only `LogNot` so `if !flag` loops stay fuse-IL
            // (`LogNotJmpt`). I2 LIR allows i64 / niche truthiness.
            if !hints.allow_match && t != MirTy::Bool {
                return Err(LowerError::Refused(format!("lnot on {t}")));
            }
            tos.push(b.ins_not(v)?);
            Ok(())
        }
        IlOp::Bin { op: inst, .. } => {
            bin_stack(b, tos, *inst)?;
            Ok(())
        }
        IlOp::BinSlotImm { op, slot, imm, .. } => {
            let inst = Instruction::from(*op);
            let ty = bin_operand_ty(inst, hints);
            let lhs = b.use_local(LocalId(u32::from(*slot)), ty)?;
            let rhs = b.ins_const(match ty {
                MirTy::I32 => MirConst::I32(i32::from(*imm)),
                MirTy::I64 => MirConst::I64(i64::from(*imm)),
                _ => {
                    return Err(LowerError::Refused("BinSlotImm on float".into()));
                }
            })?;
            tos.push(apply_bin(b, inst, lhs, rhs)?);
            Ok(())
        }
        IlOp::BinSlotSlot { op, a, b: bs, .. } => {
            let inst = Instruction::from(*op);
            let ty = bin_operand_ty(inst, hints);
            let lhs = b.use_local(LocalId(u32::from(*a)), ty)?;
            let rhs = b.use_local(LocalId(u32::from(*bs)), ty)?;
            tos.push(apply_bin(b, inst, lhs, rhs)?);
            Ok(())
        }
        IlOp::Byte { byte, .. } => lower_byte(b, tos, byte, hints),
        IlOp::Jump { .. } | IlOp::Return { .. } | IlOp::Halt { .. } => Ok(()),
        IlOp::HostInvoke { arity, layout, .. } => {
            if *layout != 0 {
                return Err(LowerError::Refused("HostInvoke layout".into()));
            }
            let n = *arity as usize;
            if tos.len() < n + 1 {
                return Err(LowerError::Refused("HostInvoke stack".into()));
            }
            let mut args = Vec::with_capacity(n);
            for _ in 0..n {
                args.push(tos.pop().expect("arity checked"));
            }
            args.reverse();
            let fn_v = tos.pop().expect("fn id");
            let id =
                const_native_id(b, fn_v).ok_or_else(|| LowerError::Refused("HostInvoke".into()))?;
            tos.push(b.ins_host_invoke(id, args)?);
            Ok(())
        }
        IlOp::Entry {
            kind: EntryKind::Call,
            arity,
            target,
            ret_words,
            ..
        } => {
            if *ret_words != 1 {
                return Err(LowerError::Refused("dense CALL is one-word".into()));
            }
            let abi = hints
                .calls
                .get(&target.0)
                .ok_or_else(|| LowerError::Refused("CALL".into()))?;
            let n = *arity as usize;
            if abi.params.len() != n {
                return Err(LowerError::Refused("CALL arity".into()));
            }
            if tos.len() < n {
                return Err(LowerError::Refused("CALL stack".into()));
            }
            let mut args = Vec::with_capacity(n);
            for _ in 0..n {
                args.push(tos.pop().expect("arity checked"));
            }
            args.reverse();
            tos.push(b.ins_call(*target, args, abi)?);
            Ok(())
        }
        IlOp::MakeTuple { arity, .. } if hints.allow_alloc => {
            lower_alloc(b, tos, MirAllocKind::Tuple, *arity)
        }
        IlOp::MakeArray { arity, .. } if hints.allow_alloc => {
            lower_alloc(b, tos, MirAllocKind::Array, *arity)
        }
        IlOp::MakeEnum { tag, arity, .. } if hints.allow_alloc => lower_alloc(
            b,
            tos,
            MirAllocKind::Enum {
                tag: u32::from(*tag),
            },
            u32::from(*arity),
        ),
        IlOp::GetField { .. }
        | IlOp::SetField { .. }
        | IlOp::LoadField { .. }
        | IlOp::MakeTuple { .. }
        | IlOp::MakeArray { .. }
        | IlOp::MakeEnum { .. }
        | IlOp::BoxValue { .. }
        | IlOp::UnboxValue { .. }
        | IlOp::Index { .. }
        | IlOp::IndexUnchecked { .. }
        | IlOp::ArrayPin { .. }
        | IlOp::IndexPin { .. }
        | IlOp::IndexPinUnchecked { .. }
        | IlOp::StoreIndexPin { .. }
        | IlOp::StoreIndexPinUnchecked { .. }
        | IlOp::Entry { .. }
        | IlOp::LoadReturnSlot { .. }
        | IlOp::ConstReturnImm { .. }
        | IlOp::BinReturn { .. }
        | IlOp::PrologueJmp { .. } => Err(LowerError::Refused(
            "non-numeric IL (classes/heap/calls stay on Value)".into(),
        )),
        IlOp::String { .. } | IlOp::Print { .. } => Err(LowerError::Refused(format!(
            "I4 {} barrier",
            refuse_reason(op).expect("string/print")
        ))),
    }
}

fn lower_byte(
    b: &mut MirBuilder,
    tos: &mut Vec<ValueId>,
    byte: &common::Byte,
    hints: &LowerHints,
) -> Result<(), LowerError> {
    match *byte.bytecode() {
        Instruction::INC | Instruction::DEC => {
            let (slot, _prefix, is_float) = byte.inc_dec_parts();
            let ty = if is_float {
                hints.default_float
            } else {
                hints.default_int
            };
            let cur = b.use_local(LocalId(slot as u32), ty)?;
            let one = if is_float {
                b.ins_const(MirConst::f64(1.0))?
            } else {
                b.ins_const(MirConst::I64(1))?
            };
            let op = if matches!(*byte.bytecode(), Instruction::DEC) {
                MirBinOp::Sub
            } else {
                MirBinOp::Add
            };
            let next = b.ins_binop(op, cur, one)?;
            b.def_local(LocalId(slot as u32), next)?;
            Ok(())
        }
        Instruction::CastIntToFloat => {
            let v = tos
                .pop()
                .ok_or_else(|| LowerError::Refused("cast stack".into()))?;
            let to = hints.default_float;
            tos.push(b.ins_cast(MirCastKind::IntToFloat, to, v)?);
            Ok(())
        }
        Instruction::NEGF | Instruction::NEG => {
            let v = tos
                .pop()
                .ok_or_else(|| LowerError::Refused("neg stack".into()))?;
            tos.push(b.ins_neg(v)?);
            Ok(())
        }
        Instruction::NOT => {
            let v = tos
                .pop()
                .ok_or_else(|| LowerError::Refused("not stack".into()))?;
            tos.push(b.ins_not(v)?);
            Ok(())
        }
        Instruction::Seek if hints.allow_match => Ok(()),
        inst if is_alloc_inst(inst) && hints.allow_alloc => {
            let (type_id, nfields) = if inst == Instruction::InitTyped {
                common::unpack_init_typed(byte.operand_u32())
            } else {
                (0, 0)
            };
            let obj = b.ins_alloc(MirAllocKind::Object { type_id, nfields }, Vec::new())?;
            let after = b.ins_gc_barrier(MirGcKind::Safepoint, vec![obj])?;
            tos.push(after);
            Ok(())
        }
        inst if is_format_inst(inst) => Err(LowerError::Refused("format".into())),
        Instruction::Unpack if hints.allow_match => {
            let arity = byte.operand_u32();
            if arity > 1 {
                return Err(LowerError::Refused("Unpack arity > 1 (I2)".into()));
            }
            let src = tos
                .pop()
                .ok_or_else(|| LowerError::Refused("Unpack stack".into()))?;
            if arity == 1 {
                let ty = match b.func().ty(src) {
                    MirTy::NicheOpt | MirTy::NicheRes | MirTy::HeapRef => MirTy::HeapRef,
                    other => other,
                };
                tos.push(b.ins_match_payload(src, 0, ty)?);
            }
            Ok(())
        }
        other => Err(LowerError::Refused(format!(
            "residual byte {}",
            other.mnemonic()
        ))),
    }
}

fn lower_alloc(
    b: &mut MirBuilder,
    tos: &mut Vec<ValueId>,
    kind: MirAllocKind,
    arity: u32,
) -> Result<(), LowerError> {
    let n = arity as usize;
    if tos.len() < n {
        return Err(LowerError::Refused("alloc stack".into()));
    }
    let mut elems = Vec::with_capacity(n);
    for _ in 0..n {
        elems.push(tos.pop().expect("arity checked"));
    }
    elems.reverse();
    let obj = b.ins_alloc(kind, elems)?;
    tos.push(b.ins_gc_barrier(MirGcKind::Safepoint, vec![obj])?);
    Ok(())
}

fn bin_stack(
    b: &mut MirBuilder,
    tos: &mut Vec<ValueId>,
    inst: Instruction,
) -> Result<(), LowerError> {
    let rhs = tos
        .pop()
        .ok_or_else(|| LowerError::Refused("bin stack".into()))?;
    let lhs = tos
        .pop()
        .ok_or_else(|| LowerError::Refused("bin stack".into()))?;
    tos.push(apply_bin(b, inst, lhs, rhs)?);
    Ok(())
}

fn const_native_id(b: &MirBuilder, v: ValueId) -> Option<u16> {
    for block in &b.func().blocks {
        for inst in &block.insts {
            if let MirInst::Const { dest, c } = inst {
                if *dest == v {
                    return match *c {
                        MirConst::I64(n) => u16::try_from(n).ok(),
                        MirConst::I32(n) => u16::try_from(n).ok(),
                        _ => None,
                    };
                }
            }
        }
    }
    None
}

fn bin_operand_ty(inst: Instruction, hints: &LowerHints) -> MirTy {
    if is_float_inst(inst) {
        hints.default_float
    } else {
        hints.default_int
    }
}

fn is_float_inst(inst: Instruction) -> bool {
    matches!(
        inst,
        Instruction::ADDF
            | Instruction::SUBF
            | Instruction::MULF
            | Instruction::DIVF
            | Instruction::MODF
            | Instruction::LEF
            | Instruction::LEQF
            | Instruction::GTF
            | Instruction::GEQF
            | Instruction::PowF
    )
}

fn apply_bin(
    b: &mut MirBuilder,
    inst: Instruction,
    lhs: ValueId,
    rhs: ValueId,
) -> Result<ValueId, LowerError> {
    if let Some(op) = map_bin(inst) {
        return Ok(b.ins_binop(op, lhs, rhs)?);
    }
    if let Some(op) = map_cmp(inst) {
        return Ok(b.ins_cmp(op, lhs, rhs)?);
    }
    Err(LowerError::Refused(format!("binop {}", inst.mnemonic())))
}

fn map_bin(inst: Instruction) -> Option<MirBinOp> {
    Some(match inst {
        Instruction::ADD | Instruction::ADDF => MirBinOp::Add,
        Instruction::SUB | Instruction::SUBF => MirBinOp::Sub,
        Instruction::MUL | Instruction::MULF => MirBinOp::Mul,
        Instruction::DIV | Instruction::DIVF => MirBinOp::Div,
        Instruction::MOD | Instruction::MODF => MirBinOp::Rem,
        Instruction::BITAND => MirBinOp::BitAnd,
        Instruction::BITOR => MirBinOp::BitOr,
        Instruction::XOR => MirBinOp::Xor,
        Instruction::SHL => MirBinOp::Shl,
        Instruction::SHR => MirBinOp::Shr,
        _ => return None,
    })
}

fn map_cmp(inst: Instruction) -> Option<MirCmpOp> {
    Some(match inst {
        Instruction::LE | Instruction::LEF => MirCmpOp::Lt,
        Instruction::LEQ | Instruction::LEQF => MirCmpOp::Le,
        Instruction::GT | Instruction::GTF => MirCmpOp::Gt,
        Instruction::GEQ | Instruction::GEQF => MirCmpOp::Ge,
        Instruction::EQ => MirCmpOp::Eq,
        Instruction::NEQ => MirCmpOp::Ne,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::DebugLoc;

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

    #[test]
    fn lowering_smoke_int_loop() {
        // i = 0; while i < n { i = i + 1 } ; return i
        let ops = vec![
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::StorePop {
                slot: 0,
                loc: loc(),
            },
            IlOp::Label(Label(0)),
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Load {
                slot: 1,
                loc: loc(),
            },
            IlOp::Bin {
                op: Instruction::LE,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: loc(),
            },
            IlOp::StorePop {
                slot: 0,
                loc: loc(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
            IlOp::Load {
                slot: 0,
                loc: loc(),
            },
            IlOp::Return {
                loc: loc(),
                ret_words: 1,
            },
        ];
        let mut hints = LowerHints::new("counted");
        hints.slot_ty.insert(0, MirTy::I64);
        hints.slot_ty.insert(1, MirTy::I64);
        let f = try_lower_numeric(&ops, &hints).expect("lower");
        f.verify().unwrap();
        assert!(
            f.blocks.iter().any(|b| b.insts.iter().any(|i| i.is_phi())),
            "loop header should carry an i64 phi"
        );
    }

    #[test]
    fn lowering_alloc_array_is_visible() {
        let loc = loc();
        let ops = vec![
            IlOp::Label(Label(0)),
            IlOp::Const { imm: 1, loc },
            IlOp::MakeArray { arity: 1, loc },
            IlOp::Return {
                loc,
                ret_words: 1,
            },
        ];
        let mut hints = LowerHints::new("arr");
        hints.allow_alloc = true;
        let f = try_lower_numeric(&ops, &hints).expect("lower alloc");
        f.verify().unwrap();
        assert!(f.has_gc_edge());
        assert!(f.blocks.iter().any(|b| {
            b.insts
                .iter()
                .any(|i| matches!(i, MirInst::Alloc { kind: MirAllocKind::Array, .. }))
        }));
        assert!(f.blocks.iter().any(|b| {
            b.insts
                .iter()
                .any(|i| matches!(i, MirInst::GcBarrier { kind: MirGcKind::Safepoint, .. }))
        }));
        assert_eq!(f.ret_ty, Some(MirTy::HeapRef));
    }

    #[test]
    fn lowering_refuses_getfield() {
        let ops = vec![IlOp::GetField { loc: loc() }];
        let err = try_lower_numeric(&ops, &LowerHints::new("cls")).unwrap_err();
        assert!(matches!(err, LowerError::Refused(_)));
    }

    #[test]
    fn lowering_refuses_format_and_string() {
        let loc = loc();
        let err =
            try_lower_numeric(&[IlOp::String { idx: 0, loc }], &LowerHints::new("s")).unwrap_err();
        assert!(matches!(err, LowerError::Refused(ref m) if m.contains("I4")));
        let err = try_lower_numeric(
            &[IlOp::Byte {
                byte: common::Byte::new(Instruction::FORMAT).with_operand_u32(1),
                loc,
            }],
            &LowerHints::new("fmt"),
        )
        .unwrap_err();
        assert!(matches!(err, LowerError::Refused(ref m) if m.contains("format")));
    }
}
