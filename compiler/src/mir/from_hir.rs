//! Lower a planned HIR body straight to MIR (HIR to MIR plan, phase M1).
//!
//! The IL lift ([`super::lower`]) rebuilds MIR from the stack IL and guesses
//! each slot's type from the opcodes that touch it. Here the types come from
//! the checker, locals are read and written by name, and control flow is
//! built from `if` / `loop` / `break` directly, so none of the operand-stack
//! replay applies.
//!
//! It covers scalar bodies (`int` / `float` / `bool` values, arithmetic,
//! compares, casts, `let` / assignment, `if`, loops and `return`), plain
//! calls, two-word pairs (enums of at most one payload word, ranges and
//! two-element tuples), frame-slot aggregates (stack arrays and class
//! locals one slot per field), each as HIR emission keeps them, and heap
//! arrays and `Vec`s of `int` / `float` that the body reads and writes but
//! does not allocate.
//! Anything else refuses with the construct's name and the body keeps the
//! IL lift.
//!
//! Each local's [`LocalId`] is the frame slot HIR lowering gave it, so the
//! sidecars keyed by IL slot (debug-slot remap, deopt maps) read the same
//! numbers as for a lifted body.

use std::collections::{HashMap, HashSet};

use common::DebugLoc;

use crate::hir::{lower, BinOp, Callee, HirArm, HirBody, HirFlags, HirId, HirKind, HirPat, HirPatFields, IndexKind, Lit, LocalId as HirLocal, MakeKind, UnOp};
use crate::il::Label;
use crate::typechecking::infer::ForInKind;
use crate::typechecking::ty::{strip_readonly, vec_element_ty, ArrayLength, Ty};

use super::abi::DenseAbi;
use super::builder::{MirBuilder, MirError};
use super::func::{MirBlock, MirFunc};
use super::inst::{BlockId, LocalId, MirAllocKind, MirBinOp, MirCastKind, MirCmpOp, MirConst, MirGcKind, MirInst, Terminator, ValueId};
use super::ty::MirTy;

/// First local of the values HIR emission keeps in temp slots.
const TEMP_BASE: u32 = 1 << 30;

/// Why a body did not lower; the name of the construct or check.
pub type Refusal = String;

/// The value an expression leaves.
#[derive(Clone, Copy)]
enum Val {
    /// A word.
    V(ValueId),
    /// `()`: nothing to read.
    Unit,
    /// Control never reaches past it (`return`, `break`, a loop with no
    /// exit).
    Never,
    /// A two-word pair: `[payload, tag]`, `[start, end]` or `[a, b]`.
    Pair(ValueId, ValueId),
}

/// A call site HIR emission lowered to a plain `CALL`.
#[derive(Clone, Debug)]
pub struct HirCallSite {
    pub target: Label,
    /// Whether each argument goes as two words.
    pub pair_args: Vec<bool>,
    /// Whether the result is two words.
    pub pair_ret: bool,
}

impl HirMirInput<'_> {
    /// The MIR type of a one-word value: [`word`], or a class object.
    fn word(&self, ty: &Ty) -> Option<MirTy> {
        word(ty).or_else(|| ((self.is_class)(ty) || is_stream(ty)).then_some(MirTy::HeapRef))
    }
}

/// Each variant's tag and payload arity, in declaration order.
pub type HostVariants = Vec<(u32, u32)>;

/// What HIR emission decided about a body, for lowering it.
pub struct HirMirInput<'a> {
    /// Frame slot of each local (the first word of a pair local).
    pub slots: &'a [Option<u32>],
    /// Second-word slot and word types of each pair local.
    pub pair_locals: HashMap<u32, (u32, MirTy, MirTy)>,
    /// Word types of each expression of a two-word type.
    pub pair_tys: HashMap<u32, (MirTy, MirTy)>,
    /// Word types of a two-word result.
    pub ret_pair: Option<(MirTy, MirTy)>,
    /// Stack arrays kept in frame slots: first slot, length and element
    /// type.
    pub stacks: HashMap<u32, (u32, usize, MirTy)>,
    /// Class locals kept in frame slots: the type of each field's slot,
    /// from the local's own.
    pub sroa: HashMap<u32, Vec<MirTy>>,
    /// The slot of each field read or written on such a class local.
    pub fields: HashMap<u32, u32>,
    pub calls: &'a HashMap<u32, HirCallSite>,
    /// `len(x)` calls: a fixed size, or `None` for the array's length.
    pub lens: &'a HashMap<u32, Option<u32>>,
    /// Statements before which frame-slot locals box (they escape there).
    pub box_at: &'a HashMap<u32, Vec<u32>>,
    /// Whether a type is a class: its value is a heap object.
    pub is_class: Box<dyn Fn(&Ty) -> bool + 'a>,
    /// Field reads and writes on heap class objects: each field's slot and
    /// word type, when the field holds the word its type says.
    pub heap_fields: HashMap<u32, (u32, MirTy)>,
    /// `Vec::push` calls HIR emission inlines.
    pub pushes: HashSet<u32>,
    /// Host native calls whose arguments and result are plain (boxed
    /// layout) words: the native id, and for a two-word enum result each
    /// variant's tag and payload arity in declaration order.
    pub hosts: HashMap<u32, (u16, Option<HostVariants>)>,
    /// `new C(..)` on the heap: the class's type id and its fields' word
    /// types.
    pub news: HashMap<u32, (u32, Vec<MirTy>)>,
    /// `for x in xs` loops whose array the checker pinned: their reads
    /// need no bounds check.
    pub pinned_for: HashSet<u32>,
    /// Whether early exits move out of line, as HIR emission's
    /// `hir_mark_cold`.
    pub cold_ok: bool,
    /// Source file of the body, for statement locations.
    pub file: Option<u32>,
}

struct Loop {
    /// Where `continue` goes: the loop head, or a `for` loop's step.
    cont: BlockId,
    /// Whether a `continue` jumped to `cont`.
    cont_used: bool,
    /// Where `break` goes, made at the first one.
    exit: Option<BlockId>,
}

struct Lower<'a> {
    hir: &'a HirBody,
    inp: &'a HirMirInput<'a>,
    b: MirBuilder,
    /// The block being filled.
    cur: BlockId,
    loops: Vec<Loop>,
    /// The function returns `()`: a `return` gives the VM's unit word.
    unit_ret: bool,
    /// Blocks in the order lowering started filling them: source order,
    /// which is how they are laid out.
    order: Vec<BlockId>,
    /// Ranges of `order` holding an early exit, laid out after the body.
    cold: Vec<(usize, usize)>,
    /// Bindings HIR emission gave no slot (an identity arm's `x` in
    /// `Some(x) => x`): never assigned, so their word is read directly.
    aliases: HashMap<u32, ValueId>,
    /// The expression being lowered is `return`ed: a `match` or `if`
    /// returns from each arm instead of joining, as HIR emission does.
    ret_tail: bool,
    /// Next local for a value HIR emission keeps in a temp slot (a `for`
    /// loop's index), numbered past any frame slot.
    next_temp: u32,
    /// Stack arrays boxed so far, in source order: the local holding each
    /// one's heap array.
    boxes: HashMap<u32, LocalId>,
}

/// The MIR type of a scalar `ty`, or `None` when it is not one.
pub fn scalar(ty: &Ty) -> Option<MirTy> {
    match lower::primitive(ty)? {
        // A byte is its value as an int word (casts mask it).
        "int" | "byte" => Some(MirTy::I64),
        "float" => Some(MirTy::F64),
        "bool" => Some(MirTy::Bool),
        _ => None,
    }
}

/// The MIR type of a one-word value: a scalar, or a heap array or `Vec`
/// of `int` or `float` (elements are raw words).
pub fn word(ty: &Ty) -> Option<MirTy> {
    scalar(ty).or_else(|| {
        matches!(array_elem(ty).and_then(scalar), Some(MirTy::I64 | MirTy::F64)).then_some(MirTy::HeapRef)
    })
}

/// An `io` stream: a heap object the GC traces like any other.
fn is_stream(ty: &Ty) -> bool {
    matches!(strip_readonly(ty), Ty::Con(n) if n == crate::typechecking::ty::STREAM)
}

/// The element type of an array or `Vec`.
fn array_elem(ty: &Ty) -> Option<&Ty> {
    match strip_readonly(ty) {
        Ty::Array { element, .. } => Some(element),
        _ => vec_element_ty(ty),
    }
}

/// A short name for the kind of `ty`, for refusal reasons.
fn ty_kind(ty: &Ty) -> &'static str {
    match strip_readonly(ty) {
        Ty::Con(n) if n == crate::typechecking::ty::STRING => "string",
        Ty::Con(n) if n == crate::typechecking::ty::BYTE => "byte",
        Ty::Con(_) => "named",
        Ty::App(head, _) => match head.as_ref() {
            Ty::Con(n) if n == "Option" => "option",
            Ty::Con(n) if n == "Result" => "result",
            Ty::Con(n) if n == common::BUILTIN_VEC_TYPE => "vec",
            _ => "generic app",
        },
        Ty::Array { .. } => "array",
        Ty::Tuple(_) => "tuple",
        Ty::Var(_) => "type variable",
        Ty::Fun(..) => "function",
        Ty::Sum { .. } | Ty::Constructor { .. } => "enum",
        _ => "other",
    }
}

fn is_unit(ty: &Ty) -> bool {
    matches!(strip_readonly(ty), Ty::Con(n) if n == crate::typechecking::ty::UNIT)
}

fn mir(e: MirError) -> Refusal {
    format!("mir: {e}")
}

/// Lower `hir` to MIR. Parameters must sit in slots `0..n`, a pair
/// parameter in two.
pub fn lower_body(hir: &HirBody, inp: &HirMirInput) -> Result<MirFunc, Refusal> {
    if hir.is_coro || hir.is_generic || !hir.captures.is_empty() {
        return Err("body kind".into());
    }
    let root = hir.root.ok_or("no root")?;
    let ret = hir.ret.as_ref().ok_or("return type")?;
    let unit_ret = is_unit(ret);
    let ret_ty = match inp.ret_pair {
        _ if unit_ret => MirTy::I64,
        Some((lo, _)) => lo,
        None => inp.word(ret).ok_or_else(|| format!("return type {}", ty_kind(ret)))?,
    };
    let mut b = MirBuilder::new(hir.name.clone());
    // Host natives are word edges, as the dense lift types them.
    b.allow_host_edges = true;
    b.set_ret_ty(ret_ty);
    let mut next = 0;
    for &param in &hir.params {
        if inp.slots.get(param.0 as usize).copied().flatten() != Some(next) {
            return Err("param slot".into());
        }
        let words = match inp.pair_locals.get(&param.0) {
            Some(&(hi, lo_ty, hi_ty)) if hi == next + 1 => vec![lo_ty, hi_ty],
            Some(_) => return Err("param slot".into()),
            None => {
                let ty = hir.local(param).ty.as_ref().ok_or("param type")?;
                vec![inp.word(ty).ok_or_else(|| format!("param type {}", ty_kind(ty)))?]
            }
        };
        for ty in words {
            let v = b.add_param(ty).map_err(mir)?;
            b.def_local(LocalId(next), v).map_err(mir)?;
            next += 1;
        }
    }
    let cur = b.entry();
    let mut lower = Lower {
        hir,
        inp,
        b,
        cur,
        loops: Vec::new(),
        unit_ret,
        order: vec![cur],
        cold: Vec::new(),
        aliases: HashMap::new(),
        ret_tail: false,
        next_temp: TEMP_BASE,
        boxes: HashMap::new(),
    };
    match lower.value(root)? {
        Val::Never => {}
        val => lower.ret(val)?,
    }
    let order = lower.layout();
    let mut func = lower.b.finish().map_err(mir)?;
    tidy(&mut func, &order);
    Ok(func)
}

/// Thread jumps through empty blocks (but not into a φ block from a
/// branch), then lay the blocks out in `order`, dropping the ones nothing
/// reaches.
fn tidy(func: &mut MirFunc, order: &[BlockId]) {
    loop {
        let mut changed = false;
        for i in 0..func.blocks.len() {
            let b = func.blocks[i].id;
            let Some(Terminator::Jump { dest: c }) = func.blocks[i].term else {
                continue;
            };
            if b == func.entry || c == b || !func.blocks[i].insts.is_empty() {
                continue;
            }
            let preds: Vec<BlockId> = func
                .blocks
                .iter()
                .filter(|p| p.term.as_ref().is_some_and(|t| t.succs().contains(&b)))
                .map(|p| p.id)
                .collect();
            let mut left = false;
            for p in preds {
                // Two edges from `p` into `c` would need two φ incomings,
                // and a branch straight into a φ block would put the φ
                // copies on a critical edge, where emit has to jump around
                // them: keep the split block there.
                let succs = func.block(p).term.as_ref().map_or(Vec::new(), |t| t.succs());
                let phis = func.block(c).insts.iter().any(|i| matches!(i, MirInst::Phi { .. }));
                if succs.contains(&c) || (succs.len() > 1 && phis) {
                    left = true;
                    continue;
                }
                retarget(func.block_mut(p).term.as_mut().expect("pred has a terminator"), b, c);
                for inst in &mut func.block_mut(c).insts {
                    if let MirInst::Phi { args, .. } = inst
                        && let Some(&(_, v)) = args.iter().find(|(from, _)| *from == b)
                    {
                        args.push((p, v));
                        args.sort_by_key(|(from, _)| *from);
                    }
                }
                changed = true;
            }
            if !left {
                for inst in &mut func.block_mut(c).insts {
                    if let MirInst::Phi { args, .. } = inst {
                        args.retain(|(from, _)| *from != b);
                    }
                }
                func.block_mut(b).term = Some(Terminator::Unreachable);
            }
        }
        if !changed {
            break;
        }
    }
    // Reachable blocks, in layout order.
    let mut reach = vec![false; func.blocks.len()];
    let mut stack = vec![func.entry];
    while let Some(b) = stack.pop() {
        if std::mem::replace(&mut reach[b.index()], true) {
            continue;
        }
        if let Some(t) = &func.block(b).term {
            stack.extend(t.succs());
        }
    }
    let mut layout: Vec<BlockId> = order.iter().copied().filter(|b| reach[b.index()]).collect();
    for b in &func.blocks {
        if reach[b.id.index()] && !layout.contains(&b.id) {
            layout.push(b.id);
        }
    }
    let mut new_id = vec![None; func.blocks.len()];
    for (i, b) in layout.iter().enumerate() {
        new_id[b.index()] = Some(BlockId(i as u32));
    }
    let map = |b: BlockId| new_id[b.index()].expect("reachable block");
    let mut old: Vec<Option<MirBlock>> = std::mem::take(&mut func.blocks).into_iter().map(Some).collect();
    for &b in &layout {
        let mut block = old[b.index()].take().expect("each block once");
        block.id = map(b);
        for inst in &mut block.insts {
            if let MirInst::Phi { args, .. } = inst {
                args.retain(|(from, _)| new_id[from.index()].is_some());
                for (from, _) in args.iter_mut() {
                    *from = map(*from);
                }
                args.sort_by_key(|(from, _)| *from);
            }
        }
        match &mut block.term {
            Some(Terminator::Jump { dest }) => *dest = map(*dest),
            Some(Terminator::Br { taken, not_taken, .. } | Terminator::JumpIfMatch { taken, not_taken, .. }) => {
                *taken = map(*taken);
                *not_taken = map(*not_taken);
            }
            _ => {}
        }
        func.blocks.push(block);
    }
    func.entry = map(func.entry);
    func.term_locs = std::mem::take(&mut func.term_locs)
        .into_iter()
        .filter_map(|(b, loc)| Some((new_id.get(b.index()).copied().flatten()?, loc)))
        .collect();
}

/// Point `term`'s edges to `from` at `to`.
fn retarget(term: &mut Terminator, from: BlockId, to: BlockId) {
    match term {
        Terminator::Jump { dest } => {
            if *dest == from {
                *dest = to;
            }
        }
        Terminator::Br { taken, not_taken, .. } | Terminator::JumpIfMatch { taken, not_taken, .. } => {
            if *taken == from {
                *taken = to;
            }
            if *not_taken == from {
                *not_taken = to;
            }
        }
        Terminator::Return { .. } | Terminator::Unreachable => {}
    }
}

impl Lower<'_> {
    fn slot(&self, local: HirLocal) -> Result<LocalId, Refusal> {
        self.inp
            .slots
            .get(local.0 as usize)
            .copied()
            .flatten()
            .map(LocalId)
            .ok_or_else(|| "local slot".into())
    }

    /// The type of a one-word local. A stack array or class local is
    /// slots, not a word.
    fn local_ty(&self, local: HirLocal) -> Result<MirTy, Refusal> {
        if self.inp.stacks.contains_key(&local.0) || self.inp.sroa.contains_key(&local.0) {
            return Err("frame-slot local value".into());
        }
        let ty = self.hir.local(local).ty.as_ref().ok_or("local type")?;
        self.inp.word(ty).ok_or_else(|| format!("local type {}", ty_kind(ty)))
    }

    fn temp(&mut self) -> LocalId {
        self.next_temp += 1;
        LocalId(self.next_temp - 1)
    }

    fn expr_ty(&self, id: HirId) -> Option<&Ty> {
        self.hir.expr(id).ty.as_ref()
    }

    fn switch(&mut self, block: BlockId) {
        self.b.switch_to_block(block);
        self.cur = block;
        if !self.order.contains(&block) {
            self.order.push(block);
        }
    }

    fn jump(&mut self, to: BlockId) -> Result<(), Refusal> {
        self.b.jump(to).map_err(mir)
    }

    fn ret_unit(&mut self) -> Result<(), Refusal> {
        let zero = self.b.ins_const(MirConst::I64(0)).map_err(mir)?;
        self.b.ret(Some(zero)).map_err(mir)
    }

    /// Return `val`, which must fit the function's result.
    fn ret(&mut self, val: Val) -> Result<(), Refusal> {
        match (val, self.inp.ret_pair) {
            (Val::Unit, _) if self.unit_ret => self.ret_unit(),
            (Val::V(v), None) if !self.unit_ret => self.b.ret(Some(v)).map_err(mir),
            (Val::Pair(lo, hi), Some((lo_ty, hi_ty))) => {
                if self.b.value_ty(lo) != lo_ty || self.b.value_ty(hi) != hi_ty {
                    return Err("return pair type".into());
                }
                self.b.ret_pair(lo, hi).map_err(mir)
            }
            // A unit body whose tail is a value, or a valued body ending in `()`.
            _ => Err("return value".into()),
        }
    }

    /// Word types of pair local `local`, with its second slot.
    fn pair_local(&self, local: HirLocal) -> Option<(LocalId, LocalId, MirTy, MirTy)> {
        let &(hi, lo_ty, hi_ty) = self.inp.pair_locals.get(&local.0)?;
        Some((self.slot(local).ok()?, LocalId(hi), lo_ty, hi_ty))
    }

    /// A zero word of `ty`: the payload of a variant that carries none.
    fn zero(&mut self, ty: MirTy) -> Result<ValueId, Refusal> {
        let c = match ty {
            MirTy::I64 => MirConst::I64(0),
            MirTy::F64 => MirConst::F64(0),
            MirTy::Bool => MirConst::Bool(false),
            _ => return Err("pair word type".into()),
        };
        self.b.ins_const(c).map_err(mir)
    }

    /// `id`'s two words; `None` when control never reaches past it.
    fn pair(&mut self, id: HirId) -> Result<Option<(ValueId, ValueId)>, Refusal> {
        match self.value(id)? {
            Val::Pair(lo, hi) => Ok(Some((lo, hi))),
            Val::Never => Ok(None),
            _ => Err("pair operand".into()),
        }
    }

    /// `id`'s word; refuses `()` and `Never` is passed up as `None`.
    fn word(&mut self, id: HirId) -> Result<Option<ValueId>, Refusal> {
        match self.value(id)? {
            Val::V(v) => Ok(Some(v)),
            Val::Never => Ok(None),
            Val::Unit => Err("unit operand".into()),
            Val::Pair(..) => Err("pair operand".into()),
        }
    }

    /// Lower `id` for its effect.
    fn effect(&mut self, id: HirId) -> Result<bool, Refusal> {
        Ok(!matches!(self.value(id)?, Val::Never))
    }

    fn value(&mut self, id: HirId) -> Result<Val, Refusal> {
        let e = self.hir.expr(id);
        let returned = std::mem::take(&mut self.ret_tail);
        match &e.kind {
            HirKind::Lit(lit) => {
                let c = match lit {
                    Lit::Int(n) => MirConst::I64(*n),
                    Lit::Float(f) => MirConst::F64(f.to_bits()),
                    Lit::Bool(v) => MirConst::Bool(*v),
                    Lit::Unit => return Ok(Val::Unit),
                    Lit::Str(raw) if self.expr_ty(id).and_then(lower::primitive) == Some(crate::typechecking::ty::BYTE) => {
                        MirConst::I64(i64::from(lower::byte_literal(raw).ok_or("byte literal")?))
                    }
                    Lit::Str(_) => return Err("string literal".into()),
                };
                // A literal typed `byte` (or anything else) is not this phase's.
                if self.expr_ty(id).and_then(scalar) != Some(c.ty()) {
                    return Err("literal type".into());
                }
                Ok(Val::V(self.b.ins_const(c).map_err(mir)?))
            }
            HirKind::Local(local) if let Some((lo, hi, lo_ty, hi_ty)) = self.pair_local(*local) => Ok(Val::Pair(
                self.b.use_local(lo, lo_ty).map_err(mir)?,
                self.b.use_local(hi, hi_ty).map_err(mir)?,
            )),
            HirKind::Local(local) if let Some(&v) = self.aliases.get(&local.0) => Ok(Val::V(v)),
            HirKind::Local(local) if let Some(&slot) = self.boxes.get(&local.0) => {
                Ok(Val::V(self.b.use_local(slot, MirTy::HeapRef).map_err(mir)?))
            }
            HirKind::Local(local) => {
                let ty = self.local_ty(*local)?;
                let slot = self.slot(*local)?;
                Ok(Val::V(self.b.use_local(slot, ty).map_err(mir)?))
            }
            HirKind::Bin { op, lhs, rhs } => self.bin(id, *op, *lhs, *rhs),
            HirKind::Call { args, .. } if let (Some(&fixed), [arg]) = (self.inp.lens.get(&id.0), args.as_slice()) => self.len(*arg, fixed),
            HirKind::Call { args, .. } if self.inp.pushes.contains(&id.0) => self.push(args),
            HirKind::Call { args, .. } if self.inp.hosts.contains_key(&id.0) => self.host(id, args),
            HirKind::Call { args, .. } => self.call(id, args),
            HirKind::Logic { and, lhs, rhs } => self.logic(*and, *lhs, *rhs),
            HirKind::Un { op, operand } => {
                let Some(v) = self.word(*operand)? else {
                    return Ok(Val::Never);
                };
                let out = match op {
                    UnOp::Neg => self.b.ins_neg(v),
                    UnOp::Not if self.b.value_ty(v) == MirTy::Bool => self.b.ins_not(v),
                    UnOp::BitNot if self.b.value_ty(v) == MirTy::I64 => {
                        let ones = self.b.ins_const(MirConst::I64(-1)).map_err(mir)?;
                        self.b.ins_binop(MirBinOp::Xor, v, ones)
                    }
                    _ => return Err("unary operand".into()),
                };
                Ok(Val::V(out.map_err(mir)?))
            }
            HirKind::Cast { value } => {
                let names = (self.expr_ty(*value).and_then(lower::primitive), self.expr_ty(id).and_then(lower::primitive));
                let from = self.expr_ty(*value).and_then(scalar);
                let to = self.expr_ty(id).and_then(scalar);
                let Some(v) = self.word(*value)? else {
                    return Ok(Val::Never);
                };
                let out = match (from, to) {
                    // `CastIntToByte` / `CastByteToInt`: the low eight bits.
                    _ if matches!(names, (Some("int" | "byte"), Some("byte")) | (Some("byte"), Some("int"))) => {
                        if names.0 == names.1 {
                            v
                        } else {
                            let mask = self.b.ins_const(MirConst::I64(0xff)).map_err(mir)?;
                            self.b.ins_binop(MirBinOp::BitAnd, v, mask).map_err(mir)?
                        }
                    }
                    _ if names.0 == Some("byte") || names.1 == Some("byte") => return Err("cast".into()),
                    (Some(a), Some(b)) if a == b => v,
                    (Some(MirTy::I64), Some(MirTy::F64)) => {
                        self.b.ins_cast(MirCastKind::IntToFloat, MirTy::F64, v).map_err(mir)?
                    }
                    (Some(MirTy::F64), Some(MirTy::I64)) => {
                        self.b.ins_cast(MirCastKind::FloatToInt, MirTy::I64, v).map_err(mir)?
                    }
                    _ => return Err("cast".into()),
                };
                Ok(Val::V(out))
            }
            HirKind::Block { stmts, tail } => {
                for &stmt in stmts {
                    if !self.stmt(stmt)? {
                        return Ok(Val::Never);
                    }
                }
                match tail {
                    // The body's own tail is a statement to HIR emission; a
                    // nested block's tail has its statement's location.
                    Some(t) if !self.b.pending_loc.is_known() => self.at_stmt(*t, |l| {
                        l.box_before(*t)?;
                        l.ret_tail = returned;
                        l.value(*t)
                    }),
                    Some(t) => {
                        self.box_before(*t)?;
                        self.ret_tail = returned;
                        self.value(*t)
                    }
                    None => Ok(Val::Unit),
                }
            }
            HirKind::Let { local, init: Some(init) } if self.inp.stacks.contains_key(&local.0) || self.inp.sroa.contains_key(&local.0) => {
                self.fill_slots(*local, *init)
            }
            HirKind::Assign { place, value }
                if let HirKind::Local(local) = self.hir.expr(*place).kind
                    && self.inp.stacks.contains_key(&local.0) =>
            {
                self.fill_slots(local, *value)
            }
            // `a[i]++` used for its value reads the old or new element.
            HirKind::Assign { .. } if e.flags.contains(HirFlags::ADJUST) && !self.expr_ty(id).is_some_and(is_unit) => {
                Err("adjust value".into())
            }
            HirKind::Assign { place, value } if let HirKind::Index { base, index, .. } = self.hir.expr(*place).kind => {
                self.stack_store(*place, base, index, *value)
            }
            HirKind::Assign { place, value } if let Some(&slot) = self.inp.fields.get(&place.0) => {
                let ty = self.expr_ty(*place).and_then(|t| self.inp.word(t)).ok_or("field type")?;
                let Some(v) = self.word(*value)? else {
                    return Ok(Val::Never);
                };
                if self.b.value_ty(v) != ty {
                    return Err("field type".into());
                }
                self.b.def_local(LocalId(slot), v).map_err(mir)?;
                Ok(Val::Unit)
            }
            HirKind::Field { base, .. } if let Some(&(slot, ty)) = self.inp.heap_fields.get(&id.0) => {
                let Some(obj) = self.word(*base)? else {
                    return Ok(Val::Never);
                };
                Ok(Val::V(self.b.ins_heap_field_load(obj, None, slot, ty).map_err(mir)?))
            }
            HirKind::Assign { place, value }
                if let HirKind::Field { base, .. } = self.hir.expr(*place).kind
                    && let Some(&(slot, ty)) = self.inp.heap_fields.get(&place.0) =>
            {
                // The value, then the object, as HIR emission's `SetField`.
                let Some(v) = self.word(*value)? else {
                    return Ok(Val::Never);
                };
                if self.b.value_ty(v) != ty {
                    return Err("field type".into());
                }
                let Some(obj) = self.word(base)? else {
                    return Ok(Val::Never);
                };
                self.b.ins_heap_field_store(obj, v, None, Some(slot)).map_err(mir)?;
                Ok(Val::Unit)
            }
            HirKind::Field { .. } if let Some(&slot) = self.inp.fields.get(&id.0) => {
                let ty = self.expr_ty(id).and_then(|t| self.inp.word(t)).ok_or("field type")?;
                Ok(Val::V(self.b.use_local(LocalId(slot), ty).map_err(mir)?))
            }
            HirKind::Index { base, index, .. }
                if let HirKind::Local(local) = self.hir.expr(*base).kind
                    && let Some(&(slot, n, ty)) = self.inp.stacks.get(&local.0)
                    && !self.boxes.contains_key(&local.0) =>
            {
                let Some(k) = self.stack_index(id, *index, n)? else {
                    return Ok(Val::Never);
                };
                let v = match k {
                    Ok(k) => self.b.use_local(LocalId(slot + k), ty).map_err(mir)?,
                    Err(idx) => self.stack_select(slot, n, idx, ty)?,
                };
                Ok(Val::V(v))
            }
            HirKind::Let { local, init } if let Some((lo, hi, lo_ty, hi_ty)) = self.pair_local(*local) => {
                let init = init.ok_or("uninitialized let")?;
                let Some((a, b)) = self.pair(init)? else {
                    return Ok(Val::Never);
                };
                if self.b.value_ty(a) != lo_ty || self.b.value_ty(b) != hi_ty {
                    return Err("let pair type".into());
                }
                self.b.def_local(lo, a).map_err(mir)?;
                self.b.def_local(hi, b).map_err(mir)?;
                Ok(Val::Unit)
            }
            // `let (a, b) = e` of a two-word tuple.
            HirKind::LetPat { pat: HirPat::Tuple(items), init } if items.len() == 2 => {
                let Some((a, b)) = self.pair(*init)? else {
                    return Ok(Val::Never);
                };
                for (item, v) in items.iter().zip([a, b]) {
                    match item {
                        HirPat::Wild => {}
                        HirPat::Bind(local) => self.bind(*local, v)?,
                        _ => return Err("let pattern".into()),
                    }
                }
                Ok(Val::Unit)
            }
            HirKind::Make { kind, args } if self.inp.pair_tys.contains_key(&id.0) => self.make_pair(id, kind, args),
            HirKind::Make { kind: MakeKind::Array, args } if self.heap_array(id) => self.make_array(id, args),
            HirKind::Make { kind: MakeKind::Class(_), args } if self.inp.news.contains_key(&id.0) => self.make_object(id, args),
            // `p[0]` / `p[1]` of a two-word tuple local.
            HirKind::Index { base, index, .. }
                if let HirKind::Local(local) = self.hir.expr(*base).kind
                    && let Some((lo, hi, lo_ty, hi_ty)) = self.pair_local(local)
                    && self.expr_ty(*base).is_some_and(|t| matches!(strip_readonly(t), Ty::Tuple(_))) =>
            {
                let (slot, ty) = match self.hir.expr(*index).kind {
                    HirKind::Lit(Lit::Int(0)) => (lo, lo_ty),
                    HirKind::Lit(Lit::Int(1)) => (hi, hi_ty),
                    _ => return Err("tuple index".into()),
                };
                Ok(Val::V(self.b.use_local(slot, ty).map_err(mir)?))
            }
            HirKind::Index { base, index, kind: IndexKind::Array } if self.heap_array(*base) => self.heap_index(id, *base, *index),
            HirKind::ForIn {
                pat: HirPat::Bind(local),
                iterable,
                body,
                kind: Some(ForInKind::Array),
            } if self.heap_array(*iterable) => self.for_array(id, *local, *iterable, *body),
            HirKind::Match { scrutinee, arms } => self.match_(*scrutinee, arms, returned),
            HirKind::Let { local, init } => {
                let init = init.ok_or("uninitialized let")?;
                self.refuse_array_copy(init)?;
                let ty = self.local_ty(*local)?;
                let slot = self.slot(*local)?;
                let Some(v) = self.word(init)? else {
                    return Ok(Val::Never);
                };
                if self.b.value_ty(v) != ty {
                    return Err("let type".into());
                }
                self.b.def_local(slot, v).map_err(mir)?;
                Ok(Val::Unit)
            }
            HirKind::Assign { place, value } => {
                // `x++` used for its value reads the old or new `x`.
                if e.flags.contains(HirFlags::ADJUST) && !self.expr_ty(id).is_some_and(is_unit) {
                    return Err("adjust value".into());
                }
                let HirKind::Local(local) = self.hir.expr(*place).kind else {
                    return Err("assign place".into());
                };
                self.refuse_array_copy(*value)?;
                let ty = self.local_ty(local)?;
                let slot = self.slot(local)?;
                let Some(v) = self.word(*value)? else {
                    return Ok(Val::Never);
                };
                if self.b.value_ty(v) != ty {
                    return Err("assign type".into());
                }
                self.b.def_local(slot, v).map_err(mir)?;
                Ok(Val::Unit)
            }
            HirKind::If { cond, then, els } => self.if_(*cond, *then, *els, returned),
            HirKind::Loop { body } => {
                let head = self.b.create_block();
                self.jump(head)?;
                self.switch(head);
                self.loops.push(Loop {
                    cont: head,
                    cont_used: false,
                    exit: None,
                });
                let live = self.effect(*body);
                let done = self.loops.pop().expect("loop pushed above");
                if live? {
                    self.jump(head)?;
                }
                // Nothing leaves a loop with no `break`.
                let Some(exit) = done.exit else {
                    return Ok(Val::Never);
                };
                self.switch(exit);
                Ok(Val::Unit)
            }
            HirKind::Break => {
                let exit = match self.loops.last().ok_or("break outside loop")?.exit {
                    Some(exit) => exit,
                    None => {
                        let exit = self.b.create_block();
                        self.loops.last_mut().expect("checked above").exit = Some(exit);
                        exit
                    }
                };
                self.jump(exit)?;
                Ok(Val::Never)
            }
            HirKind::Continue => {
                let lp = self.loops.last_mut().ok_or("continue outside loop")?;
                lp.cont_used = true;
                let cont = lp.cont;
                self.jump(cont)?;
                Ok(Val::Never)
            }
            HirKind::ForIn {
                pat,
                iterable,
                body,
                kind: Some(ForInKind::Range { inclusive, float }),
            } => {
                let HirPat::Bind(local) = *pat else {
                    return Err("for in pattern".into());
                };
                self.for_range(local, *iterable, *body, *inclusive, *float)
            }
            HirKind::Return(value) => {
                match value {
                    None if self.unit_ret => self.ret_unit()?,
                    Some(v) if self.unit_ret => {
                        if !self.effect(*v)? {
                            return Ok(Val::Never);
                        }
                        self.ret_unit()?;
                    }
                    Some(v) => {
                        self.ret_tail = true;
                        match self.value(*v)? {
                            Val::Never => return Ok(Val::Never),
                            val => self.ret(val)?,
                        }
                    }
                    None => return Err("return value".into()),
                }
                Ok(Val::Never)
            }
            other => Err(kind_name(other).into()),
        }
    }

    /// `for x in lo..hi`, as HIR lowering emits it: `x` is the counter
    /// when the body never assigns it, `hi` is read once, and the step
    /// adds one after the body (or at a `continue`).
    fn for_range(&mut self, local: HirLocal, iterable: HirId, body: HirId, inclusive: bool, float: bool) -> Result<Val, Refusal> {
        let bounds = lower::range_bounds(self.hir, iterable);
        let ty = self.local_ty(local)?;
        if ty != if float { MirTy::F64 } else { MirTy::I64 } {
            return Err("for in range type".into());
        }
        let x = self.slot(local)?;
        if !float && let Some((start, trips)) = lower::unrolled_range(self.hir, iterable, body, inclusive) {
            // Unrolled: each value into `x`, then the body.
            for k in 0..trips {
                let v = self.b.ins_const(MirConst::I64(start + i64::from(k))).map_err(mir)?;
                self.b.def_local(x, v).map_err(mir)?;
                if !self.effect(body)? {
                    return Ok(Val::Never);
                }
            }
            return Ok(Val::Unit);
        }
        if lower::assigns_local(self.hir, body, local) {
            return Err("for in counter assigned".into());
        }
        // `a..b` written in place, or a range pair (a local or a call).
        let (start, end) = match bounds {
            Some([lo, hi]) => {
                let Some(start) = self.word(lo)? else {
                    return Ok(Val::Never);
                };
                let Some(end) = self.word(hi)? else {
                    return Ok(Val::Never);
                };
                (start, end)
            }
            None => match self.pair(iterable)? {
                Some(pair) => pair,
                None => return Ok(Val::Never),
            },
        };
        if self.b.value_ty(start) != ty || self.b.value_ty(end) != ty {
            return Err("for in bound type".into());
        }
        self.b.def_local(x, start).map_err(mir)?;
        let head = self.b.create_block();
        let exit = self.b.create_block();
        let body_b = self.b.create_block();
        self.jump(head)?;
        self.switch(head);
        let cur = self.b.use_local(x, ty).map_err(mir)?;
        let op = if inclusive { MirCmpOp::Le } else { MirCmpOp::Lt };
        let more = self.b.ins_cmp(op, cur, end).map_err(mir)?;
        self.b.branch(more, body_b, exit).map_err(mir)?;
        self.switch(body_b);
        let one = if float { MirConst::F64(1.0_f64.to_bits()) } else { MirConst::I64(1) };
        self.counted_body(body, head, exit, x, ty, one)
    }

    /// A counted loop's body, then `counter += one` and back to `head`. The
    /// step ends the body's last block unless a `continue` needs it as its
    /// own target, as HIR emission places it (the shape MIR vectorize
    /// matches).
    fn counted_body(&mut self, body: HirId, head: BlockId, exit: BlockId, counter: LocalId, ty: MirTy, one: MirConst) -> Result<Val, Refusal> {
        let step = lower::has_own_continue(self.hir, body).then(|| self.b.create_block());
        self.loops.push(Loop {
            cont: step.unwrap_or(head),
            cont_used: false,
            exit: Some(exit),
        });
        let live = self.effect(body);
        let done = self.loops.pop().expect("loop pushed above");
        let live = live?;
        match step {
            Some(step) if live || done.cont_used => {
                if live {
                    self.jump(step)?;
                }
                self.switch(step);
            }
            None if live => {}
            // Nothing reaches the step.
            _ => {
                self.switch(exit);
                return Ok(Val::Unit);
            }
        }
        let cur = self.b.use_local(counter, ty).map_err(mir)?;
        let one = self.b.ins_const(one).map_err(mir)?;
        let next = self.b.ins_binop(MirBinOp::Add, cur, one).map_err(mir)?;
        self.b.def_local(counter, next).map_err(mir)?;
        self.jump(head)?;
        self.switch(exit);
        Ok(Val::Unit)
    }

    fn bin(&mut self, id: HirId, op: BinOp, lhs: HirId, rhs: HirId) -> Result<Val, Refusal> {
        let arith = match op {
            BinOp::IntAdd | BinOp::FloatAdd => Some(MirBinOp::Add),
            BinOp::IntSub | BinOp::FloatSub => Some(MirBinOp::Sub),
            BinOp::IntMul | BinOp::FloatMul => Some(MirBinOp::Mul),
            BinOp::IntDiv | BinOp::FloatDiv => Some(MirBinOp::Div),
            BinOp::IntRem | BinOp::FloatRem => Some(MirBinOp::Rem),
            BinOp::Shl => Some(MirBinOp::Shl),
            BinOp::Shr => Some(MirBinOp::Shr),
            BinOp::BitAnd => Some(MirBinOp::BitAnd),
            BinOp::BitOr => Some(MirBinOp::BitOr),
            BinOp::BitXor => Some(MirBinOp::Xor),
            _ => None,
        };
        let cmp = match op {
            BinOp::Eq => Some(MirCmpOp::Eq),
            BinOp::Ne => Some(MirCmpOp::Ne),
            BinOp::Lt => Some(MirCmpOp::Lt),
            BinOp::Le => Some(MirCmpOp::Le),
            BinOp::Gt => Some(MirCmpOp::Gt),
            BinOp::Ge => Some(MirCmpOp::Ge),
            _ => None,
        };
        if arith.is_none() && cmp.is_none() {
            return Err(format!("operator {op:?}"));
        }
        let Some(l) = self.word(lhs)? else {
            return Ok(Val::Never);
        };
        let Some(r) = self.word(rhs)? else {
            return Ok(Val::Never);
        };
        let lt = self.b.value_ty(l);
        // `==` on arrays compares elements; only scalars compare as words.
        if !lt.is_numeric() {
            return Err("operator operand type".into());
        }
        let out = match (arith, cmp) {
            (Some(MirBinOp::BitAnd | MirBinOp::BitOr), _) if lt == MirTy::Bool => {
                self.b.ins_bool_logic(arith.expect("matched"), l, r)
            }
            (Some(arith), _) => {
                if self.expr_ty(id).and_then(scalar) != Some(lt) {
                    return Err("operator type".into());
                }
                self.b.ins_binop(arith, l, r)
            }
            (None, Some(cmp)) => self.b.ins_cmp(cmp, l, r),
            (None, None) => unreachable!("refused above"),
        };
        Ok(Val::V(out.map_err(mir)?))
    }

    fn logic(&mut self, and: bool, lhs: HirId, rhs: HirId) -> Result<Val, Refusal> {
        let Some(l) = self.word(lhs)? else {
            return Ok(Val::Never);
        };
        if lower::logic_eager(self.hir, rhs) {
            // `b` has no effect and cannot trap: one `&` / `|`, as HIR
            // lowering emits it.
            let Some(r) = self.word(rhs)? else {
                return Ok(Val::Never);
            };
            let op = if and { MirBinOp::BitAnd } else { MirBinOp::BitOr };
            return Ok(Val::V(self.b.ins_bool_logic(op, l, r).map_err(mir)?));
        }
        // `a && b` is `if a { b } else { false }`; `a || b` is
        // `if a { true } else { b }`.
        let short = self.b.ins_const(MirConst::Bool(!and)).map_err(mir)?;
        let from = self.cur;
        let long = self.b.create_block();
        let join = self.b.create_block();
        if and {
            self.b.branch(l, long, join).map_err(mir)?;
        } else {
            self.b.branch(l, join, long).map_err(mir)?;
        }
        self.switch(long);
        let mut args = vec![(from, short)];
        if let Some(r) = self.word(rhs)? {
            args.push((self.cur, r));
            self.jump(join)?;
        }
        self.switch(join);
        Ok(Val::V(self.b.ins_stack_phi(args).map_err(mir)?))
    }

    /// `id` as a block statement: what it lowers carries the statement's
    /// source location, as HIR emission gives its IL ops (line breakpoints,
    /// source views). Nested statements keep their own.
    fn stmt(&mut self, id: HirId) -> Result<bool, Refusal> {
        self.at_stmt(id, |l| {
            l.box_before(id)?;
            l.effect(id)
        })
    }

    /// Box the stack arrays that escape first in statement (or block tail)
    /// `id` into one heap array each, as HIR emission's `hir_box_before`:
    /// every use after it in source order goes through the array.
    fn box_before(&mut self, id: HirId) -> Result<(), Refusal> {
        let Some(locals) = self.inp.box_at.get(&id.0) else {
            return Ok(());
        };
        for &local in locals {
            let Some(&(base, n, ty)) = self.inp.stacks.get(&local) else {
                return Err("boxed class local".into());
            };
            if !matches!(ty, MirTy::I64 | MirTy::F64) {
                return Err("boxed stack array type".into());
            }
            let mut items = Vec::with_capacity(n);
            for i in 0..n as u32 {
                items.push(self.b.use_local(LocalId(base + i), ty).map_err(mir)?);
            }
            let arr = self.b.ins_alloc(MirAllocKind::Array, items).map_err(mir)?;
            let arr = self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![arr]).map_err(mir)?;
            let slot = self.temp();
            self.b.def_local(slot, arr).map_err(mir)?;
            self.boxes.insert(local, slot);
        }
        Ok(())
    }

    /// The heap array a boxed stack array local `id` names.
    fn boxed(&self, id: HirId) -> Option<LocalId> {
        match self.hir.expr(id).kind {
            HirKind::Local(local) => self.boxes.get(&local.0).copied(),
            _ => None,
        }
    }

    /// `f` with the source location of statement `id`.
    fn at_stmt<T>(&mut self, id: HirId, f: impl FnOnce(&mut Self) -> Result<T, Refusal>) -> Result<T, Refusal> {
        let outer = self.b.pending_loc;
        if let Some(file) = self.inp.file {
            let (start, end) = self.hir.expr(id).span;
            self.b.pending_loc = DebugLoc {
                file,
                start_byte: start as u32,
                end_byte: end.max(start + 1) as u32,
            };
        }
        let out = f(self);
        self.b.pending_loc = outer;
        out
    }

    /// A plain call: `CALL` of the entry label HIR emission used, with the
    /// word ABI of the argument and result types (a pair as two words).
    fn call(&mut self, id: HirId, args: &[HirId]) -> Result<Val, Refusal> {
        let Some(site) = self.inp.calls.get(&id.0) else {
            return Err(match &self.hir.expr(id).kind {
                HirKind::Call { callee: Callee::Named { name, .. } | Callee::Method { name }, .. } => format!("call `{name}`"),
                _ => "call value".to_string(),
            });
        };
        if site.pair_args.len() != args.len() {
            return Err("call arity".into());
        }
        let ty = self.expr_ty(id).ok_or("call type")?;
        let unit = is_unit(ty);
        let (ret, ret_hi) = match self.inp.pair_tys.get(&id.0) {
            Some(&(lo, hi)) if site.pair_ret => (lo, Some(hi)),
            _ if site.pair_ret => return Err("call type".into()),
            _ if unit => (MirTy::I64, None),
            _ => (self.inp.word(ty).ok_or_else(|| format!("call type {}", ty_kind(ty)))?, None),
        };
        let mut words = Vec::with_capacity(args.len());
        for (&arg, &pair) in args.iter().zip(&site.pair_args) {
            if !pair && self.expr_ty(arg).and_then(|t| self.inp.word(t)).is_none() {
                return Err("call argument type".into());
            }
            match (self.value(arg)?, pair) {
                (Val::Never, _) => return Ok(Val::Never),
                (Val::V(v), false) => words.push(v),
                (Val::Pair(lo, hi), true) => words.extend([lo, hi]),
                _ => return Err("call argument".into()),
            }
        }
        let abi = DenseAbi {
            params: words.iter().map(|&v| self.b.value_ty(v)).collect(),
            ret,
            ret_hi,
        };
        let (dest, dest_hi) = self.b.ins_call(site.target, words, &abi).map_err(mir)?;
        Ok(match dest_hi {
            Some(hi) => Val::Pair(dest, hi),
            None if unit => Val::Unit,
            None => Val::V(dest),
        })
    }

    /// Fill the slots of stack array or class local `local` from `init`:
    /// each item or field into its own slot, or another stack array's slots
    /// copied, as HIR emission's `hir_stack_array_init`.
    fn fill_slots(&mut self, local: HirLocal, init: HirId) -> Result<Val, Refusal> {
        if self.boxes.contains_key(&local.0) {
            return Err("refill boxed stack array".into());
        }
        let base = self.slot(local)?.0;
        let tys = match (self.inp.stacks.get(&local.0), self.inp.sroa.get(&local.0)) {
            (Some(&(_, n, ty)), _) => vec![ty; n],
            (None, Some(tys)) => tys.clone(),
            (None, None) => return Err("frame-slot local".into()),
        };
        match &self.hir.expr(init).kind {
            HirKind::Make {
                kind: MakeKind::Array | MakeKind::Class(_),
                args,
            } if args.len() == tys.len() => {
                // Every item before any slot changes (`a = [a[1], a[0]]`).
                let mut words = Vec::with_capacity(args.len());
                for (&arg, &ty) in args.iter().zip(&tys) {
                    let Some(v) = self.word(arg)? else {
                        return Ok(Val::Never);
                    };
                    if self.b.value_ty(v) != ty {
                        return Err("frame-slot item type".into());
                    }
                    words.push(v);
                }
                for (i, v) in words.into_iter().enumerate() {
                    self.b.def_local(LocalId(base + i as u32), v).map_err(mir)?;
                }
            }
            HirKind::Local(src) if let Some(&(from, n, ty)) = self.inp.stacks.get(&src.0) && n == tys.len() => {
                for i in 0..n as u32 {
                    let v = self.b.use_local(LocalId(from + i), ty).map_err(mir)?;
                    self.b.def_local(LocalId(base + i), v).map_err(mir)?;
                }
            }
            _ => return Err("frame-slot init".into()),
        }
        Ok(Val::Unit)
    }

    /// A stack array's index: `Ok(k)` for a literal in range, else the
    /// index word when it needs no bounds check (`None` when control never
    /// gets past it). An index that might be out of range refuses: HIR
    /// emission boxes the array to panic there.
    #[allow(clippy::type_complexity)]
    fn stack_index(&mut self, node: HirId, index: HirId, n: usize) -> Result<Option<Result<u32, ValueId>>, Refusal> {
        if let HirKind::Lit(Lit::Int(i)) = self.hir.expr(index).kind
            && (0..n as i64).contains(&i)
        {
            return Ok(Some(Ok(i as u32)));
        }
        if !lower::stack_index_proven(self.hir, node, index, n) {
            return Err("stack index".into());
        }
        Ok(self.index_word(index)?.map(Err))
    }

    /// An index's word, with HIR emission's Euclidean fix-up for `x % m`:
    /// `r + ((r >> 63) & m)`.
    fn index_word(&mut self, index: HirId) -> Result<Option<ValueId>, Refusal> {
        let Some(mut idx) = self.word(index)? else {
            return Ok(None);
        };
        if self.b.value_ty(idx) != MirTy::I64 {
            return Err("index type".into());
        }
        if let Some(m) = lower::index_euclid(self.hir, index) {
            let sh = self.b.ins_const(MirConst::I64(63)).map_err(mir)?;
            let m = self.b.ins_const(MirConst::I64(i64::from(m))).map_err(mir)?;
            let sign = self.b.ins_binop(MirBinOp::Shr, idx, sh).map_err(mir)?;
            let fix = self.b.ins_binop(MirBinOp::BitAnd, sign, m).map_err(mir)?;
            idx = self.b.ins_binop(MirBinOp::Add, idx, fix).map_err(mir)?;
        }
        Ok(Some(idx))
    }

    /// HIR emission copies a fixed-size array local bound or assigned to
    /// another (`vec_from_array`); that host call is not lowered here.
    fn refuse_array_copy(&self, value: HirId) -> Result<(), Refusal> {
        let HirKind::Local(src) = self.hir.expr(value).kind else {
            return Ok(());
        };
        let copied = !self.inp.stacks.contains_key(&src.0)
            && matches!(self.expr_ty(value).map(strip_readonly), Some(Ty::Array { length, .. }) if *length != ArrayLength::Dynamic);
        if copied {
            return Err("fixed array copy".into());
        }
        Ok(())
    }

    /// Whether `id` is a heap array this phase lowers (its type is one word).
    fn heap_array(&self, id: HirId) -> bool {
        self.expr_ty(id).and_then(word) == Some(MirTy::HeapRef)
    }

    /// `xs[i]` on a heap array: the array, then the index, as HIR emission
    /// pushes them.
    fn heap_index(&mut self, id: HirId, base: HirId, index: HirId) -> Result<Val, Refusal> {
        let ty = self.expr_ty(id).and_then(scalar).ok_or("element type")?;
        let Some(arr) = self.word(base)? else {
            return Ok(Val::Never);
        };
        let Some(idx) = self.index_word(index)? else {
            return Ok(Val::Never);
        };
        // A boxed stack array's read is always checked, as HIR emission's.
        let proven = self.hir.expr(id).flags.contains(HirFlags::IN_BOUNDS) && self.boxed(base).is_none();
        Ok(Val::V(self.b.ins_index(arr, idx, ty, proven).map_err(mir)?))
    }

    /// `xs[i] = v` on a heap array: the value first, then the array and
    /// index, as HIR emission stages them. The bounds check stays unless
    /// the checker proved the index and the value cannot change the array.
    fn heap_store(&mut self, place: HirId, base: HirId, index: HirId, value: HirId) -> Result<Val, Refusal> {
        let ty = self.expr_ty(place).and_then(scalar).ok_or("element type")?;
        let Some(v) = self.word(value)? else {
            return Ok(Val::Never);
        };
        if self.b.value_ty(v) != ty {
            return Err("element type".into());
        }
        let Some(arr) = self.word(base)? else {
            return Ok(Val::Never);
        };
        let Some(idx) = self.index_word(index)? else {
            return Ok(Val::Never);
        };
        let mut quiet = true;
        lower::visit(self.hir, value, &mut |e| {
            quiet &= !matches!(
                e.kind,
                HirKind::Call { .. } | HirKind::Assign { .. } | HirKind::Append { .. } | HirKind::Yield { .. } | HirKind::Resume { .. }
            );
        });
        let unchecked = quiet && self.hir.expr(place).flags.contains(HirFlags::IN_BOUNDS) && self.boxed(base).is_none();
        self.b.ins_store_index(arr, idx, v, unchecked).map_err(mir)?;
        Ok(Val::Unit)
    }

    /// `[a, b, ..]` on the heap: each item, then the array. The array word
    /// is the safepoint's, as the IL lift reads it.
    fn make_array(&mut self, id: HirId, args: &[HirId]) -> Result<Val, Refusal> {
        let ty = self.expr_ty(id).and_then(array_elem).and_then(scalar).ok_or("element type")?;
        let mut items = Vec::with_capacity(args.len());
        for &arg in args {
            let Some(v) = self.word(arg)? else {
                return Ok(Val::Never);
            };
            if self.b.value_ty(v) != ty {
                return Err("element type".into());
            }
            items.push(v);
        }
        let arr = self.b.ins_alloc(MirAllocKind::Array, items).map_err(mir)?;
        Ok(Val::V(self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![arr]).map_err(mir)?))
    }

    /// `new C(..)`: every argument, then the object and each field's store
    /// (no argument sees the object, so none runs with it allocated).
    fn make_object(&mut self, id: HirId, args: &[HirId]) -> Result<Val, Refusal> {
        let (type_id, tys) = &self.inp.news[&id.0];
        let (type_id, tys) = (*type_id, tys.clone());
        let mut words = Vec::with_capacity(args.len());
        for (&arg, ty) in args.iter().zip(tys) {
            let Some(v) = self.word(arg)? else {
                return Ok(Val::Never);
            };
            if self.b.value_ty(v) != ty {
                return Err("field type".into());
            }
            words.push(v);
        }
        let nfields = words.len() as u32;
        let obj = self.b.ins_alloc(MirAllocKind::Object { type_id, nfields }, Vec::new()).map_err(mir)?;
        let obj = self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![obj]).map_err(mir)?;
        for (slot, v) in words.into_iter().enumerate() {
            self.b.ins_heap_field_store(obj, v, None, Some(slot as u32)).map_err(mir)?;
        }
        Ok(Val::V(obj))
    }

    /// `v.push(x)` on a heap `Vec` of int/float: the Vec, then the value,
    /// then `ArrayPush` (a safepoint: it may grow the storage).
    fn push(&mut self, args: &[HirId]) -> Result<Val, Refusal> {
        let &[recv, value] = args else {
            return Err("push arity".into());
        };
        let ty = self.expr_ty(recv).and_then(array_elem).and_then(scalar);
        if !self.heap_array(recv) {
            return Err("push receiver".into());
        }
        let Some(arr) = self.word(recv)? else {
            return Ok(Val::Never);
        };
        let Some(v) = self.word(value)? else {
            return Ok(Val::Never);
        };
        if Some(self.b.value_ty(v)) != ty {
            return Err("push value type".into());
        }
        let out = self.b.ins_array_push(arr, v).map_err(mir)?;
        self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![out]).map_err(mir)?;
        Ok(Val::Unit)
    }

    /// A host native: the arguments in order, then `HostInvoke` (the native
    /// id is the instruction's, not a value the lowering has to find).
    fn host(&mut self, id: HirId, args: &[HirId]) -> Result<Val, Refusal> {
        let (native, unbox) = &self.inp.hosts[&id.0];
        let native = *native;
        let pair = match (self.inp.pair_tys.get(&id.0), unbox) {
            (Some(&tys), Some(variants)) => Some((tys, variants.clone())),
            (Some(_), None) => return Err("host pair result".into()),
            (None, _) => None,
        };
        let unit = self.expr_ty(id).is_some_and(is_unit);
        let ret = match self.expr_ty(id) {
            _ if pair.is_some() => Some(MirTy::HeapRef),
            _ if unit => None,
            Some(ty) => Some(self.inp.word(ty).ok_or_else(|| format!("host result {}", ty_kind(ty)))?),
            None => return Err("host result".into()),
        };
        let mut words = Vec::with_capacity(args.len());
        for &arg in args {
            let Some(v) = self.word(arg)? else {
                return Ok(Val::Never);
            };
            words.push(v);
        }
        let out = self.b.ins_host_invoke_as(native, words, 0, ret).map_err(mir)?;
        if let Some(((lo_ty, _), variants)) = pair {
            return self.unbox_pair(out, lo_ty, &variants);
        }
        Ok(if unit { Val::Unit } else { Val::V(out) })
    }

    /// A boxed enum word to `[payload, tag]`, as HIR emission's
    /// `emit_unbox_enum_to_pair`: a `JumpIfMatch` per variant but the last,
    /// which the fallthrough unpacks.
    fn unbox_pair(&mut self, obj: ValueId, lo_ty: MirTy, variants: &[(u32, u32)]) -> Result<Val, Refusal> {
        let Some((&(last_tag, last_arity), rest)) = variants.split_last() else {
            return Err("host pair variants".into());
        };
        let mut out = Vec::with_capacity(variants.len());
        for &(t, arity) in rest {
            let word = match arity {
                0 => None,
                1 => Some(self.b.ins_match_payload(obj, 0, lo_ty).map_err(mir)?),
                _ => return Err("host pair arity".into()),
            };
            let (hit, miss) = (self.b.create_block(), self.b.create_block());
            self.b.jump_if_match(obj, t, word.into_iter().collect(), hit, miss).map_err(mir)?;
            self.switch(hit);
            let p = match word {
                Some(w) => w,
                None => self.zero(lo_ty)?,
            };
            let tag = self.b.ins_const(MirConst::I64(i64::from(t))).map_err(mir)?;
            out.push((self.cur, Val::Pair(p, tag)));
            self.switch(miss);
        }
        let p = match last_arity {
            0 => self.zero(lo_ty)?,
            1 => self.b.ins_match_payload(obj, 0, lo_ty).map_err(mir)?,
            _ => return Err("host pair arity".into()),
        };
        let tag = self.b.ins_const(MirConst::I64(i64::from(last_tag))).map_err(mir)?;
        out.push((self.cur, Val::Pair(p, tag)));
        self.join_arms(out)
    }

    /// `len(x)`: a fixed size (a local or literal is not read), or the
    /// heap array's length.
    fn len(&mut self, arg: HirId, fixed: Option<u32>) -> Result<Val, Refusal> {
        if let Some(n) = fixed {
            if !matches!(self.hir.expr(arg).kind, HirKind::Local(_) | HirKind::Lit(_) | HirKind::Make { .. }) {
                return Err("len operand".into());
            }
            return Ok(Val::V(self.b.ins_const(MirConst::I64(i64::from(n))).map_err(mir)?));
        }
        if !self.heap_array(arg) {
            return Err("len operand".into());
        }
        let Some(arr) = self.word(arg)? else {
            return Ok(Val::Never);
        };
        Ok(Val::V(self.b.ins_array_len(arr).map_err(mir)?))
    }

    /// `for x in xs` over a heap array, as HIR emission lowers it: the
    /// length is read once, and the index steps after the body (or at a
    /// `continue`).
    fn for_array(&mut self, id: HirId, local: HirLocal, iterable: HirId, body: HirId) -> Result<Val, Refusal> {
        let ty = self.local_ty(local)?;
        let elem = self.expr_ty(iterable).and_then(array_elem).and_then(scalar);
        if elem != Some(ty) {
            return Err("for in element type".into());
        }
        let x = self.slot(local)?;
        let Some(arr) = self.word(iterable)? else {
            return Ok(Val::Never);
        };
        let len = self.b.ins_array_len(arr).map_err(mir)?;
        let i = self.temp();
        let zero = self.b.ins_const(MirConst::I64(0)).map_err(mir)?;
        self.b.def_local(i, zero).map_err(mir)?;
        let head = self.b.create_block();
        let exit = self.b.create_block();
        let body_b = self.b.create_block();
        self.jump(head)?;
        self.switch(head);
        let cur = self.b.use_local(i, MirTy::I64).map_err(mir)?;
        let more = self.b.ins_cmp(MirCmpOp::Lt, cur, len).map_err(mir)?;
        self.b.branch(more, body_b, exit).map_err(mir)?;
        self.switch(body_b);
        let cur = self.b.use_local(i, MirTy::I64).map_err(mir)?;
        let item = self.b.ins_index(arr, cur, ty, self.inp.pinned_for.contains(&id.0)).map_err(mir)?;
        self.b.def_local(x, item).map_err(mir)?;
        self.counted_body(body, head, exit, i, MirTy::I64, MirConst::I64(1))
    }

    /// Branch on `idx` to each of slots `base..base + n`, as HIR emission's
    /// select: every slot but the last is tested. `at` runs in each case.
    fn stack_cases(&mut self, base: u32, n: usize, idx: ValueId, mut at: impl FnMut(&mut Self, LocalId) -> Result<Option<ValueId>, Refusal>) -> Result<Vec<(BlockId, ValueId)>, Refusal> {
        let mut out = Vec::new();
        let mut ends = Vec::new();
        for k in 0..n {
            let next = if k + 1 < n {
                let c = self.b.ins_const(MirConst::I64(k as i64)).map_err(mir)?;
                let hit = self.b.ins_cmp(MirCmpOp::Eq, idx, c).map_err(mir)?;
                let take = self.b.create_block();
                let next = self.b.create_block();
                self.b.branch(hit, take, next).map_err(mir)?;
                self.switch(take);
                Some(next)
            } else {
                None
            };
            if let Some(v) = at(self, LocalId(base + k as u32))? {
                out.push((self.cur, v));
            }
            ends.push(self.cur);
            if let Some(next) = next {
                self.switch(next);
            }
        }
        // The join after every case block, as an `if` makes it.
        let join = self.b.create_block();
        for end in ends {
            self.b.switch_to_block(end);
            self.jump(join)?;
        }
        self.switch(join);
        Ok(out)
    }

    /// The element of stack array slots `base..base + n` at `idx`.
    fn stack_select(&mut self, base: u32, n: usize, idx: ValueId, ty: MirTy) -> Result<ValueId, Refusal> {
        let args = self.stack_cases(base, n, idx, |l, slot| Ok(Some(l.b.use_local(slot, ty).map_err(mir)?)))?;
        self.b.ins_stack_phi(args).map_err(mir)
    }

    /// `a[i] = v` on a stack array: the value first, then the index, as
    /// HIR emission stages them.
    fn stack_store(&mut self, place: HirId, base: HirId, index: HirId, value: HirId) -> Result<Val, Refusal> {
        let stack = match self.hir.expr(base).kind {
            HirKind::Local(local) if !self.boxes.contains_key(&local.0) => self.inp.stacks.get(&local.0).copied(),
            _ => None,
        };
        let Some((slot, n, ty)) = stack else {
            if self.heap_array(base) {
                return self.heap_store(place, base, index, value);
            }
            return Err("assign place".into());
        };
        let Some(v) = self.word(value)? else {
            return Ok(Val::Never);
        };
        if self.b.value_ty(v) != ty {
            return Err("stack item type".into());
        }
        let Some(k) = self.stack_index(place, index, n)? else {
            return Ok(Val::Never);
        };
        match k {
            Ok(k) => self.b.def_local(LocalId(slot + k), v).map_err(mir)?,
            Err(idx) => {
                self.stack_cases(slot, n, idx, |l, slot| {
                    l.b.def_local(slot, v).map_err(mir)?;
                    Ok(None)
                })?;
            }
        }
        Ok(Val::Unit)
    }

    /// Bind `local` to the word `v`.
    fn bind(&mut self, local: HirLocal, v: ValueId) -> Result<(), Refusal> {
        if self.local_ty(local)? != self.b.value_ty(v) {
            return Err("binding type".into());
        }
        if self.inp.slots.get(local.0 as usize).copied().flatten().is_none() {
            let assigned = self.hir.exprs.iter().any(|e| {
                matches!(e.kind, HirKind::Assign { place, .. } if matches!(self.hir.expr(place).kind, HirKind::Local(l) if l == local))
            });
            if assigned {
                return Err("local slot".into());
            }
            self.aliases.insert(local.0, v);
            return Ok(());
        }
        let slot = self.slot(local)?;
        self.b.def_local(slot, v).map_err(mir)
    }

    /// A variant, range or tuple built as two words.
    fn make_pair(&mut self, id: HirId, kind: &MakeKind, args: &[HirId]) -> Result<Val, Refusal> {
        let (lo_ty, hi_ty) = self.inp.pair_tys[&id.0];
        let (lo, hi) = match kind {
            MakeKind::Variant { tag: Some(tag), .. } => {
                let lo = match args {
                    [] => self.zero(lo_ty)?,
                    [arg] => match self.word(*arg)? {
                        Some(v) if self.b.value_ty(v) == lo_ty => v,
                        Some(_) => return Err("variant payload type".into()),
                        None => return Ok(Val::Never),
                    },
                    _ => return Err("variant payload".into()),
                };
                (lo, self.b.ins_const(MirConst::I64(i64::from(*tag))).map_err(mir)?)
            }
            MakeKind::Range { .. } | MakeKind::Tuple => {
                let [a, b] = args else {
                    return Err("make pair".into());
                };
                let Some(a) = self.word(*a)? else {
                    return Ok(Val::Never);
                };
                let Some(b) = self.word(*b)? else {
                    return Ok(Val::Never);
                };
                (a, b)
            }
            _ => return Err("make pair".into()),
        };
        if self.b.value_ty(lo) != lo_ty || self.b.value_ty(hi) != hi_ty {
            return Err("make pair type".into());
        }
        Ok(Val::Pair(lo, hi))
    }

    /// `match` on a `[payload, tag]` pair: each arm but the last tests the
    /// tag, as HIR emission's `hir_match_pair`.
    fn match_(&mut self, scrutinee: HirId, arms: &[HirArm], tail: bool) -> Result<Val, Refusal> {
        // Arms up to the first catch-all; later ones never run.
        let reach = arms
            .iter()
            .position(|a| matches!(a.pat, HirPat::Wild | HirPat::Bind(_)))
            .map_or(arms.len(), |i| i + 1);
        let arms = &arms[..reach];
        if lower::has_nested_test(arms) {
            return Err("nested match".into());
        }
        let (payload, tag) = match self.value(scrutinee)? {
            Val::Pair(payload, tag) => (payload, tag),
            Val::Never => return Ok(Val::Never),
            _ => return Err("match scrutinee".into()),
        };
        // `Some(x) => x, None => 0`: a unit variant's payload word is `0`,
        // so the value is the payload whatever the tag.
        let zero_arm = |arm: &HirArm| {
            matches!(arm.pat, HirPat::Variant { fields: HirPatFields::Unit, .. })
                && matches!(self.hir.expr(arm.body).kind, HirKind::Lit(Lit::Int(0)))
        };
        if let [a, b] = arms
            && (zero_arm(a) && lower::is_identity_arm(self.hir, b) || zero_arm(b) && lower::is_identity_arm(self.hir, a))
            && self.b.value_ty(payload) == MirTy::I64
        {
            return Ok(Val::V(payload));
        }
        let last = arms.len() - 1;
        let mut out = Vec::new();
        let mut last_start = None;
        for (i, arm) in arms.iter().enumerate() {
            let miss = if i < last {
                let HirPat::Variant { tag: Some(t), .. } = arm.pat else {
                    return Err("match arm pattern".into());
                };
                let t = self.b.ins_const(MirConst::I64(i64::from(t))).map_err(mir)?;
                let hit = self.b.ins_cmp(MirCmpOp::Eq, tag, t).map_err(mir)?;
                let arm_b = self.b.create_block();
                let miss = self.b.create_block();
                self.b.branch(hit, arm_b, miss).map_err(mir)?;
                self.switch(arm_b);
                Some(miss)
            } else {
                None
            };
            self.bind_payload(&arm.pat, payload)?;
            let val = self.arm_value(arm.body, tail)?;
            if !matches!(val, Val::Never) {
                out.push((self.cur, val));
            }
            if let Some(miss) = miss {
                last_start = Some(self.order.len());
                self.switch(miss);
            }
        }
        // A last arm that only exits (`?`'s `Err(e) => return Err(e)`) is cold.
        if let Some(start) = last_start
            && lower::cold_exit(self.hir, arms[last].body)
        {
            self.mark_cold(start);
        }
        self.join_arms(out)
    }

    /// Bind a match arm's payload names to the payload word.
    fn bind_payload(&mut self, pat: &HirPat, payload: ValueId) -> Result<(), Refusal> {
        let HirPat::Variant { fields, .. } = pat else {
            return match pat {
                HirPat::Wild => Ok(()),
                _ => Err("match arm pattern".into()),
            };
        };
        let bound = match fields {
            HirPatFields::Unit => return Ok(()),
            HirPatFields::Tuple(items) => match items.as_slice() {
                [] | [HirPat::Wild] => return Ok(()),
                [HirPat::Bind(local)] => *local,
                _ => return Err("payload pattern".into()),
            },
            HirPatFields::Record(items) => match items.as_slice() {
                [] | [(_, HirPat::Wild)] => return Ok(()),
                [(_, HirPat::Bind(local))] => *local,
                _ => return Err("payload pattern".into()),
            },
        };
        self.bind(bound, payload)
    }

    /// The value where the arms reaching the current block meet: one φ per
    /// word.
    fn join(&mut self, arms: Vec<(BlockId, Val)>) -> Result<Val, Refusal> {
        if arms.iter().all(|(_, v)| matches!(v, Val::Unit)) {
            return Ok(Val::Unit);
        }
        let mut lo = Vec::new();
        let mut hi = Vec::new();
        for (block, val) in arms {
            match val {
                Val::V(v) => lo.push((block, v)),
                Val::Pair(a, b) => {
                    lo.push((block, a));
                    hi.push((block, b));
                }
                _ => return Err("arm value".into()),
            }
        }
        if !hi.is_empty() && hi.len() != lo.len() {
            return Err("arm value".into());
        }
        let mut phi = |args: Vec<(BlockId, ValueId)>| -> Result<ValueId, Refusal> {
            let ty = self.b.value_ty(args[0].1);
            if args.iter().any(|&(_, v)| self.b.value_ty(v) != ty) {
                return Err("arm types".into());
            }
            self.b.ins_stack_phi(args).map_err(mir)
        };
        let lo = phi(lo)?;
        Ok(if hi.is_empty() { Val::V(lo) } else { Val::Pair(lo, phi(hi)?) })
    }

    /// Lay out the blocks lowering started since `order[start]` after the
    /// body; a range nested in one already marked goes with it.
    fn mark_cold(&mut self, start: usize) {
        if self.inp.cold_ok {
            self.cold.retain(|&(s, _)| s < start);
            self.cold.push((start, self.order.len()));
        }
    }

    /// The blocks in layout order: source order with the cold ranges last.
    fn layout(&mut self) -> Vec<BlockId> {
        let order = std::mem::take(&mut self.order);
        let cold = |i: usize| self.cold.iter().any(|&(s, e)| (s..e).contains(&i));
        let mut out: Vec<BlockId> = (0..order.len()).filter(|&i| !cold(i)).map(|i| order[i]).collect();
        for &(s, e) in &self.cold {
            out.extend_from_slice(&order[s..e]);
        }
        out
    }

    /// The value of an arm body; with `tail`, returned from the arm.
    fn arm_value(&mut self, body: HirId, tail: bool) -> Result<Val, Refusal> {
        self.ret_tail = tail;
        match self.value(body)? {
            Val::Never => Ok(Val::Never),
            val if tail => {
                self.ret(val)?;
                Ok(Val::Never)
            }
            val => Ok(val),
        }
    }

    fn if_(&mut self, cond: HirId, then: HirId, els: Option<HirId>, tail: bool) -> Result<Val, Refusal> {
        // `if !c` branches on `c` with the edges swapped, as HIR lowering
        // inverts it.
        let (cond, negated) = match self.hir.expr(cond).kind {
            HirKind::Un { op: UnOp::Not, operand } => (operand, true),
            _ => (cond, false),
        };
        let Some(c) = self.word(cond)? else {
            return Ok(Val::Never);
        };
        if self.b.value_ty(c) != MirTy::Bool {
            return Err("if condition".into());
        }
        let then_b = self.b.create_block();
        let else_b = self.b.create_block();
        if negated {
            self.b.branch(c, else_b, then_b).map_err(mir)?;
        } else {
            self.b.branch(c, then_b, else_b).map_err(mir)?;
        }
        let mut arms = Vec::new();
        for (block, arm) in [(then_b, Some(then)), (else_b, els)] {
            let start = self.order.len();
            self.switch(block);
            let val = match arm {
                Some(arm) => self.arm_value(arm, tail)?,
                None if tail => {
                    self.ret(Val::Unit)?;
                    Val::Never
                }
                None => Val::Unit,
            };
            // The arm a statement `if` leaves by (`then` without an `else`,
            // else the `else`) is cold when it only exits.
            let exit_arm = if els.is_some() { arm == els } else { arm == Some(then) };
            if matches!(val, Val::Never) && exit_arm && arm.is_some_and(|a| lower::cold_exit(self.hir, a)) {
                self.mark_cold(start);
            }
            if !matches!(val, Val::Never) {
                arms.push((self.cur, val));
            }
        }
        self.join_arms(arms)
    }

    /// Jump from each arm that falls through to a join made after all of
    /// them, and give the value there; `Never` when every arm leaves.
    fn join_arms(&mut self, arms: Vec<(BlockId, Val)>) -> Result<Val, Refusal> {
        if arms.is_empty() {
            return Ok(Val::Never);
        }
        let join = self.b.create_block();
        for &(block, _) in &arms {
            self.b.switch_to_block(block);
            self.jump(join)?;
        }
        self.switch(join);
        self.join(arms)
    }
}

/// A short name for a construct this phase does not lower.
fn kind_name(kind: &HirKind) -> &'static str {
    match kind {
        HirKind::Global { .. } => "global",
        HirKind::Field { .. } => "field",
        HirKind::Index { .. } => "index",
        HirKind::Call { .. } => "call",
        HirKind::Named { .. } | HirKind::Spread(_) => "argument",
        HirKind::Make { .. } => "make",
        HirKind::LetPat { .. } => "let pattern",
        HirKind::Append { .. } => "append",
        HirKind::ForIn { .. } => "for in",
        HirKind::Match { .. } => "match",
        HirKind::Lambda { .. } => "lambda",
        HirKind::Yield { .. } | HirKind::Resume { .. } => "coroutine",
        HirKind::Defer { .. } => "defer",
        HirKind::Builtin { .. } => "builtin",
        HirKind::Clear(_) => "clear",
        HirKind::Unsupported(_) => "unsupported",
        _ => "construct",
    }
}
