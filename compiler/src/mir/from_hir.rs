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

use crate::hir::{lower, BinOp, Builtin, Callee, HirArm, HirBody, HirFlags, HirId, HirKind, HirPat, HirPatFields, IndexKind, Lit, LocalId as HirLocal, MakeKind, UnOp};
use crate::il::Label;
use crate::typechecking::infer::{ForInCounted, ForInKind};
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
    /// A `gen fn` / `async fn` call: `MakeCoro`, its result the handle.
    pub coro: bool,
}

impl HirMirInput<'_> {
    /// The MIR type of a one-word value: [`word`], a class object or a
    /// scalar enum's backing word.
    fn word(&self, ty: &Ty) -> Option<MirTy> {
        word(ty)
            .or_else(|| ((self.is_class)(ty) || is_record(ty) || is_stream(ty) || is_string(ty) || is_coroutine(ty)).then_some(MirTy::HeapRef))
            .or_else(|| (self.scalar_enum)(ty))
            .or_else(|| (self.niche)(ty))
            .or_else(|| (self.boxed_enum)(ty, None).map(|_| MirTy::HeapRef))
            .or_else(|| array_elem(ty).and_then(|e| self.heap_elem(e)).map(|_| MirTy::HeapRef))
    }

    /// An array element type held as a heap word: a string, class, array,
    /// boxed enum or boxed tuple.
    fn heap_elem(&self, elem: &Ty) -> Option<MirTy> {
        let heap = self.word(elem) == Some(MirTy::HeapRef)
            || matches!(strip_readonly(elem), Ty::Tuple(items) if !items.is_empty())
            || (self.boxed_view)(elem, None).is_some();
        heap.then_some(MirTy::HeapRef)
    }

    /// A heap word stored in a tuple or array slot as the value itself:
    /// not a niche word (stored boxed).
    fn plain_heap(&self, ty: &Ty) -> bool {
        (self.word(ty) == Some(MirTy::HeapRef) && (self.niche)(ty).is_none())
            || matches!(strip_readonly(ty), Ty::Tuple(items) if !items.is_empty())
    }

    /// The word type of an array's elements that direct MIR reads and
    /// writes: `int` or `float`, or a heap word.
    fn elem_word(&self, arr: &Ty) -> Option<MirTy> {
        let elem = array_elem(arr)?;
        match scalar(elem) {
            Some(t @ (MirTy::I64 | MirTy::F64)) => Some(t),
            _ => self.heap_elem(elem),
        }
    }
}

/// The backing word type of a scalar enum type.
pub type ScalarEnumFn<'a> = Box<dyn Fn(&Ty) -> Option<MirTy> + 'a>;

/// A scalar enum variant's backing constant, by enum and variant name.
pub type ScalarVariantFn<'a> = Box<dyn Fn(&str, &str) -> Option<MirConst> + 'a>;

/// Each variant's tag and payload arity, in declaration order.
pub type HostVariants = Vec<(u32, u32)>;

/// For a boxed user enum type (one word, an `ObjEnum`): with no variant,
/// `Some` of nothing; with a variant, its payload field types in
/// declaration order. `None` for every other type.
pub type BoxedEnumFn<'a> = Box<dyn Fn(&Ty, Option<&str>) -> Option<Vec<Ty>> + 'a>;

/// A variant's payload field names, by enum and variant name.
pub type PayloadNamesFn<'a> = Box<dyn Fn(&str, &str) -> Vec<String> + 'a>;

/// A two-word enum's name and its variants' tags and payload word types.
pub type PairVariantsFn<'a> = Box<dyn Fn(&Ty) -> Option<(String, Vec<(u32, Option<MirTy>)>)> + 'a>;

/// The word of a pointer-niche `Option` or `Result` type:
/// [`MirTy::NicheOpt`] when the zero word is one side (`None`, `Ok(())`),
/// [`MirTy::NicheRes`] when `Err` is the pointer with its low bit set.
pub type NicheFn<'a> = Box<dyn Fn(&Ty) -> Option<MirTy> + 'a>;

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
    /// The user `into_iter` entry each counted `for` loop calls.
    pub into_iters: &'a HashMap<u32, Label>,
    /// String table index of each `string` literal.
    pub strings: &'a HashMap<u32, u32>,
    /// `format` calls with only word arguments, and string `+` (as
    /// `"%s%s"`): the format string's index.
    pub formats: &'a HashMap<u32, u32>,
    /// `len(x)` calls: a fixed size, or `None` for the array's length.
    pub lens: &'a HashMap<u32, Option<u32>>,
    /// Statements before which frame-slot locals box (they escape there).
    pub box_at: &'a HashMap<u32, Vec<u32>>,
    /// Whether a type is a class: its value is a heap object.
    pub is_class: Box<dyn Fn(&Ty) -> bool + 'a>,
    /// The backing word of a scalar enum type (`#[repr(int)]` /
    /// `#[repr(float)]`).
    pub scalar_enum: ScalarEnumFn<'a>,
    /// A scalar enum variant's backing constant, by enum and variant name.
    pub scalar_variant: ScalarVariantFn<'a>,
    /// Boxed user enums: a heap word, matched with `JumpIfMatch`.
    pub boxed_enum: BoxedEnumFn<'a>,
    /// A user enum whose variants carry a drop hook (`TagEnumType` after
    /// `MakeEnum`), by name.
    pub enum_drop: Box<dyn Fn(&str) -> bool + 'a>,
    /// The enum a two-word enum type pairs (not a tuple or range) and its
    /// variants' tags and payload word types (none for no payload), in
    /// declaration order.
    pub pair_variants: PairVariantsFn<'a>,
    /// A variant's payload field names in declaration order, by enum and
    /// variant name (a record pattern's fields in word order).
    pub payload_names: PayloadNamesFn<'a>,
    /// Nested matches dispatch on their outer tag first, as HIR emission's
    /// `hir_match_grouped` (`COIL_HIR_MATCH_TREE`).
    pub match_tree: bool,
    /// Like `boxed_enum` for any enum, `Option` or `Result` type: the view
    /// of a value of it boxed to one word (a pair kept in one local).
    pub boxed_view: BoxedEnumFn<'a>,
    /// Pointer-niche `Option` / `Result` words.
    pub niche: NicheFn<'a>,
    /// Field reads and writes on heap class objects: each field's slot and
    /// word type, when the field holds the word its type says.
    pub heap_fields: HashMap<u32, (u32, MirTy)>,
    /// Field reads and writes on records (dicts): the key's string table
    /// index and the field's word type.
    pub record_fields: HashMap<u32, (u32, MirTy)>,
    /// Record literals whose every field holds the word its type says:
    /// each field's key index and word type, in literal order.
    pub records: HashMap<u32, Vec<(u32, MirTy)>>,
    /// `Vec::push` calls HIR emission inlines.
    pub pushes: HashSet<u32>,
    /// Host native calls whose arguments and result are plain (boxed
    /// layout) words: the native id, its result layout, and for a two-word enum result each
    /// variant's tag and payload arity in declaration order.
    pub hosts: HashMap<u32, (u16, u8, Option<HostVariants>)>,
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
    /// Payloads read from a niche word: the word, and whether it was an
    /// untagged `Err`. Making the same variant again gives the word back.
    unniche: HashMap<ValueId, (ValueId, bool)>,
    /// Values made ahead of need (a niche payload a rewrap never reads),
    /// dropped when unused.
    spec: HashSet<ValueId>,
    /// Each word retyped by a `Word` cast, by the cast's value.
    word_src: HashMap<ValueId, ValueId>,
    /// The next host call leaves its boxed enum result boxed (a `match`
    /// dispatches on it directly).
    raw_host: bool,
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

/// A variant pattern with no payload: `V` or `V()`.
fn unit_fields(fields: &HirPatFields) -> bool {
    match fields {
        HirPatFields::Unit => true,
        HirPatFields::Tuple(parts) => parts.is_empty(),
        HirPatFields::Record(_) => false,
    }
}

/// An `io` stream: a heap object the GC traces like any other.
fn is_string(ty: &Ty) -> bool {
    matches!(strip_readonly(ty), Ty::Con(n) if n == crate::typechecking::ty::STRING)
}

/// What a counted array loop walks.
#[derive(Clone, Copy)]
enum Items {
    Array,
    /// A dict's `(key, value)` entries.
    Dict,
    /// A tuple's items, gathered into an array.
    Tuple(usize),
}

/// A record (dict) value: one heap word.
fn is_record(ty: &Ty) -> bool {
    matches!(strip_readonly(ty), Ty::Record { .. })
}

/// A coroutine handle type, `coroutine<Y, R>`: one heap word.
fn is_coroutine(ty: &Ty) -> bool {
    matches!(strip_readonly(ty), Ty::App(head, args) if args.len() == 2 && matches!(head.as_ref(), Ty::Con(n) if n == "coroutine"))
}

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
    match strip_readonly(ty) {
        Ty::Con(n) => n == crate::typechecking::ty::UNIT,
        // A function declared with no return type.
        Ty::Tuple(items) => items.is_empty(),
        _ => false,
    }
}

fn mir(e: MirError) -> Refusal {
    format!("mir: {e}")
}

/// Lower `hir` to MIR. Parameters must sit in slots `0..n`, a pair
/// parameter in two.
pub fn lower_body(hir: &HirBody, inp: &HirMirInput) -> Result<MirFunc, Refusal> {
    if hir.is_coro {
        return Err("body kind coroutine".into());
    }
    if hir.is_generic {
        return Err("body kind generic".into());
    }
    if !hir.captures.is_empty() {
        return Err("body kind closure".into());
    }
    let root = hir.root.ok_or("no root")?;
    let ret = hir.ret.as_ref().ok_or("return type")?;
    let unit_ret = is_unit(ret);
    let ret_ty = match inp.ret_pair {
        _ if unit_ret => MirTy::I64,
        Some((lo, _)) => lo,
        // A tuple not returned as a pair comes back boxed.
        None if matches!(strip_readonly(ret), Ty::Tuple(items) if !items.is_empty()) => MirTy::HeapRef,
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
                // A two-word type or tuple not passed as a pair comes boxed.
                let word = inp
                    .word(ty)
                    .or_else(|| (inp.boxed_view)(ty, None).map(|_| MirTy::HeapRef))
                    .or_else(|| matches!(strip_readonly(ty), Ty::Tuple(items) if !items.is_empty()).then_some(MirTy::HeapRef));
                vec![word.ok_or_else(|| format!("param type {}", ty_kind(ty)))?]
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
        unniche: HashMap::new(),
        spec: HashSet::new(),
        word_src: HashMap::new(),
        raw_host: false,
    };
    match lower.value(root)? {
        Val::Never => {}
        val => lower.ret(val)?,
    }
    let order = lower.layout();
    let spec = std::mem::take(&mut lower.spec);
    let mut func = lower.b.finish().map_err(mir)?;
    prune(&mut func, &spec);
    tidy(&mut func, &order);
    Ok(func)
}

/// Drop the values in `spec` nothing uses.
fn prune(func: &mut MirFunc, spec: &HashSet<ValueId>) {
    loop {
        let mut used = HashSet::new();
        for block in &func.blocks {
            for inst in &block.insts {
                used.extend(inst.operands());
            }
            if let Some(term) = &block.term {
                term.clone().rewrite_values(|v| {
                    used.insert(v);
                    v
                });
            }
        }
        let mut changed = false;
        for block in &mut func.blocks {
            let n = block.insts.len();
            block.insts.retain(|i| !spec.contains(&i.dest()) || used.contains(&i.dest()));
            changed |= block.insts.len() != n;
        }
        if !changed {
            return;
        }
    }
}

/// Thread jumps through empty blocks (but not into a φ block from a
/// branch), then lay the blocks out in `order`, dropping the ones nothing
/// reaches.
fn tidy(func: &mut MirFunc, order: &[BlockId]) {
    // A branch whose arms are both empty and end alike (a jump to one block
    // with the same φ values from each, or the same return) goes the same
    // way either way: jump to one arm.
    for i in 0..func.blocks.len() {
        let Some(Terminator::Br { taken, not_taken, .. }) = func.blocks[i].term else {
            continue;
        };
        let empty = |x: BlockId| x != func.entry && func.block(x).insts.is_empty();
        if taken == not_taken || !empty(taken) || !empty(not_taken) {
            continue;
        }
        let same = match (&func.block(taken).term, &func.block(not_taken).term) {
            (Some(Terminator::Jump { dest: c }), Some(Terminator::Jump { dest: c2 })) => {
                c == c2
                    && func.block(*c).insts.iter().all(|inst| match inst {
                        MirInst::Phi { args, .. } => {
                            let from = |x: BlockId| args.iter().find(|(f, _)| *f == x).map(|&(_, v)| v);
                            from(taken) == from(not_taken)
                        }
                        _ => true,
                    })
            }
            (Some(t @ Terminator::Return { .. }), Some(t2)) => t == t2,
            _ => false,
        };
        if same {
            func.blocks[i].term = Some(Terminator::Jump { dest: taken });
        }
    }
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
        self.inp
            .word(ty)
            .or_else(|| self.boxed_local(local).then_some(MirTy::HeapRef))
            // A tuple not kept as a pair is a heap tuple.
            .or_else(|| {
                (!self.inp.pair_locals.contains_key(&local.0) && matches!(strip_readonly(ty), Ty::Tuple(items) if !items.is_empty()))
                    .then_some(MirTy::HeapRef)
            })
            .ok_or_else(|| format!("local type {}", ty_kind(ty)))
    }

    /// A local of a two-word type that HIR emission keeps boxed in one slot.
    fn boxed_local(&self, local: HirLocal) -> bool {
        !self.inp.pair_locals.contains_key(&local.0)
            && self.hir.local(local).ty.as_ref().is_some_and(|t| (self.inp.boxed_view)(t, None).is_some())
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
            // A unit body's tail value (a call kept for its effect) is dropped.
            (Val::V(_) | Val::Pair(..), _) if self.unit_ret => self.ret_unit(),
            (Val::V(v), None) if !self.unit_ret => self.b.ret(Some(v)).map_err(mir),
            // A two-word value the function returns boxed.
            (Val::Pair(lo, hi), None) if self.b.func().ret_ty == Some(MirTy::HeapRef) => {
                let ty = self.hir.ret.clone().ok_or("return type")?;
                match self.box_pair_val(&ty, lo, hi)? {
                    Val::V(v) => self.b.ret(Some(v)).map_err(mir),
                    _ => Err("return value".into()),
                }
            }
            // A boxed value the function returns as a pair.
            (Val::V(v), Some((lo_ty, hi_ty))) if self.b.value_ty(v) == MirTy::HeapRef => {
                let ty = self.hir.ret.clone().ok_or("return type")?;
                match self.unbox_word(&ty, v, lo_ty, hi_ty)? {
                    Val::Pair(lo, hi) => self.b.ret_pair(lo, hi).map_err(mir),
                    _ => Err("return value".into()),
                }
            }
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

    /// `return` of a one-word function with no value: `0`, as HIR
    /// emission's `CONST 0; RETURN`.
    fn ret_zero(&mut self) -> Result<(), Refusal> {
        let ty = self.b.func().ret_ty.ok_or("return value")?;
        let z = self.b.ins_const(MirConst::I64(0)).map_err(mir)?;
        let z = self.fit(z, ty)?;
        self.b.ret(Some(z)).map_err(mir)
    }

    /// Word types of pair local `local`, with its second slot.
    fn pair_local(&self, local: HirLocal) -> Option<(LocalId, LocalId, MirTy, MirTy)> {
        let &(hi, lo_ty, hi_ty) = self.inp.pair_locals.get(&local.0)?;
        Some((self.slot(local).ok()?, LocalId(hi), lo_ty, hi_ty))
    }

    /// `v` as a word of type `ty`: itself, or the same word retyped (a heap
    /// payload moves through a pair as an opaque `i64`).
    fn fit(&mut self, v: ValueId, ty: MirTy) -> Result<ValueId, Refusal> {
        // Through an earlier retype: one cast, or none, from the source.
        let v = match self.word_src.get(&v) {
            Some(&src) if self.b.value_ty(src) == ty || MirCastKind::Word.allows(self.b.value_ty(src), ty) => src,
            _ => v,
        };
        let from = self.b.value_ty(v);
        if from == ty {
            return Ok(v);
        }
        if !MirCastKind::Word.allows(from, ty) {
            return Err(format!("word {from} as {ty}"));
        }
        let out = self.b.ins_cast(MirCastKind::Word, ty, v).map_err(mir)?;
        self.word_src.insert(out, v);
        Ok(out)
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
            // A boxed word (an element read, a boxed local) unboxed.
            Val::V(v) if self.b.value_ty(v) == MirTy::HeapRef && let Some(&(lo_ty, hi_ty)) = self.inp.pair_tys.get(&id.0) => {
                let ty = self.expr_ty(id).ok_or("pair operand")?.clone();
                match self.unbox_word(&ty, v, lo_ty, hi_ty)? {
                    Val::Pair(lo, hi) => Ok(Some((lo, hi))),
                    Val::Never => Ok(None),
                    _ => Err("pair operand".into()),
                }
            }
            _ => Err("pair operand".into()),
        }
    }

    /// `id`'s word, a two-word value boxed to one, as HIR emission's
    /// conversion of a pair to a boxed local or return.
    fn boxed_word(&mut self, id: HirId) -> Result<Option<ValueId>, Refusal> {
        match self.value(id)? {
            Val::V(v) => Ok(Some(v)),
            Val::Never => Ok(None),
            Val::Unit => Err("unit operand".into()),
            Val::Pair(lo, hi) => {
                let ty = self.expr_ty(id).ok_or("pair operand")?.clone();
                match self.box_pair_val(&ty, lo, hi)? {
                    Val::V(v) => Ok(Some(v)),
                    _ => Err("pair operand".into()),
                }
            }
        }
    }

    /// A boxed word of two-word type `ty` as its pair, as HIR emission's
    /// `emit_unbox_product_to_pair` / `emit_unbox_enum_to_pair`.
    fn unbox_word(&mut self, ty: &Ty, v: ValueId, lo_ty: MirTy, hi_ty: MirTy) -> Result<Val, Refusal> {
        if let Ty::Tuple(items) = strip_readonly(ty) {
            if items.len() != 2 {
                return Err("unbox pair type".into());
            }
            let zero = self.b.ins_const(MirConst::I64(0)).map_err(mir)?;
            let lo = self.b.ins_index(v, zero, lo_ty, false).map_err(mir)?;
            let one = self.b.ins_const(MirConst::I64(1)).map_err(mir)?;
            let hi = self.b.ins_index(v, one, hi_ty, false).map_err(mir)?;
            return Ok(Val::Pair(lo, hi));
        }
        // A boxed numeric range: a typed object, start and end its fields.
        if crate::typechecking::return_layout::two_word_range_kind(ty).is_some() {
            let lo = self.b.ins_heap_field_load(v, None, 0, lo_ty).map_err(mir)?;
            let hi = self.b.ins_heap_field_load(v, None, 1, hi_ty).map_err(mir)?;
            return Ok(Val::Pair(lo, hi));
        }
        if hi_ty != MirTy::I64 {
            return Err("unbox pair type".into());
        }
        let (_, variants) = (self.inp.pair_variants)(ty).ok_or("unbox pair type")?;
        let variants: Vec<(u32, u32)> = variants.iter().map(|&(t, w)| (t, u32::from(w.is_some()))).collect();
        self.unbox_pair(v, lo_ty, &variants)
    }

    /// A boxed tuple: its fields in order, then `Alloc`.
    fn make_tuple(&mut self, args: &[HirId]) -> Result<Val, Refusal> {
        let mut words = Vec::with_capacity(args.len());
        for &arg in args {
            let Some(v) = self.word(arg)? else {
                return Ok(Val::Never);
            };
            words.push(v);
        }
        let obj = self.b.ins_alloc(MirAllocKind::Tuple, words).map_err(mir)?;
        Ok(Val::V(self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![obj]).map_err(mir)?))
    }

    /// A pair of type `ty` boxed: a two-field tuple, or an enum value.
    fn box_pair_val(&mut self, ty: &Ty, lo: ValueId, hi: ValueId) -> Result<Val, Refusal> {
        if let Ty::Tuple(items) = strip_readonly(ty) {
            if items.len() != 2 {
                return Err("box pair type".into());
            }
            let obj = self.b.ins_alloc(MirAllocKind::Tuple, vec![lo, hi]).map_err(mir)?;
            return Ok(Val::V(self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![obj]).map_err(mir)?));
        }
        // A numeric range: its typed object, start and end its fields, as
        // HIR emission's `emit_box_range_slots`.
        if let Some(kind) = crate::typechecking::return_layout::two_word_range_kind(ty) {
            use crate::typechecking::return_layout as rl;
            let inclusive = rl::range_kind_inclusive(kind).ok_or("box pair type")?;
            let type_id = rl::range_heap_type_id(inclusive);
            let obj = self.b.ins_alloc(MirAllocKind::Object { type_id, nfields: 2 }, Vec::new()).map_err(mir)?;
            let obj = self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![obj]).map_err(mir)?;
            self.b.ins_heap_field_store(obj, lo, None, Some(0)).map_err(mir)?;
            self.b.ins_heap_field_store(obj, hi, None, Some(1)).map_err(mir)?;
            return Ok(Val::V(obj));
        }
        self.box_pair(ty, lo, hi)
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
        // A host's two-word result nobody reads stays the boxed word it
        // comes back as: no unboxing to a pair just to drop it.
        self.raw_host = self.host_pair(id) || self.boxed_call(id);
        let out = self.value(id);
        self.raw_host = false;
        Ok(!matches!(out?, Val::Never))
    }

    /// A host call whose result is a boxed enum unboxed to a pair.
    fn host_pair(&self, id: HirId) -> bool {
        matches!(self.hir.expr(id).kind, HirKind::Call { .. })
            && self.inp.hosts.get(&id.0).is_some_and(|h| h.2.is_some())
            && self.inp.pair_tys.contains_key(&id.0)
    }

    /// A call whose two-word result comes back boxed (`Vec::pop`'s thunk).
    fn boxed_call(&self, id: HirId) -> bool {
        self.inp.calls.get(&id.0).is_some_and(|site| !site.pair_ret) && self.inp.pair_tys.contains_key(&id.0)
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
                    Lit::Str(_) => match self.inp.strings.get(&id.0) {
                        Some(&idx) if self.expr_ty(id).is_some_and(is_string) => {
                            return Ok(Val::V(self.b.ins_string(idx).map_err(mir)?));
                        }
                        _ => return Err("string literal".into()),
                    },
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
            // `ok` bound from `Ok(())` (a `?` on a unit result) is `()`.
            HirKind::Local(local) if self.hir.local(*local).ty.as_ref().is_some_and(is_unit) => Ok(Val::Unit),
            HirKind::Local(local) if let Some(&slot) = self.boxes.get(&local.0) => {
                Ok(Val::V(self.b.use_local(slot, MirTy::HeapRef).map_err(mir)?))
            }
            HirKind::Local(local) => {
                let ty = self.local_ty(*local)?;
                let slot = self.slot(*local)?;
                Ok(Val::V(self.b.use_local(slot, ty).map_err(mir)?))
            }
            // `a + b` on strings is `format("%s%s", a, b)`.
            HirKind::Bin { op: BinOp::StrConcat, lhs, rhs } if let Some(&idx) = self.inp.formats.get(&id.0) => self.format(idx, &[*lhs, *rhs]),
            HirKind::Bin { op, lhs, rhs } => self.bin(id, *op, *lhs, *rhs),
            HirKind::Call { args, .. } if let (Some(&fixed), [arg]) = (self.inp.lens.get(&id.0), args.as_slice()) => self.len(*arg, fixed),
            HirKind::Call { args, .. } if self.inp.pushes.contains(&id.0) => self.push(args),
            HirKind::Call { args, .. } if let Some(&idx) = self.inp.formats.get(&id.0) => self.format(idx, args.get(1..).unwrap_or_default()),
            HirKind::Call { args, .. } if self.inp.hosts.contains_key(&id.0) => self.host(id, args),
            HirKind::Call { args, .. } => self.call(id, args),
            HirKind::Resume { handle, value } => self.resume(id, *handle, *value),
            HirKind::Builtin { op: Builtin::Done, args } if let [handle] = args.as_slice() => {
                let Some(h) = self.word(*handle)? else {
                    return Ok(Val::Never);
                };
                Ok(Val::V(self.b.ins_done_coro(h).map_err(mir)?))
            }
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
                // A `()` block's host-call tail is dropped (`main`'s
                // `write(..)`): its result stays raw.
                if let Some(t) = *tail
                    && self.expr_ty(id).is_some_and(is_unit)
                    && (self.host_pair(t) || self.boxed_call(t))
                {
                    self.raw_host = true;
                }
                match tail {
                    // The body's own tail is a statement to HIR emission, and
                    // so is a `return`, `break` or `continue` ending a nested
                    // block (an `if` arm's `return lo;`); any other nested
                    // tail has its statement's location.
                    Some(t)
                        if !self.b.pending_loc.is_known()
                            || matches!(self.hir.expr(*t).kind, HirKind::Return(_) | HirKind::Break | HirKind::Continue) =>
                    {
                        self.at_stmt(*t, |l| {
                            l.box_before(*t)?;
                            l.ret_tail = returned;
                            l.value(*t)
                        })
                    }
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
            // A record's field by key: the object, then the key, as HIR
            // emission's `GetField`.
            HirKind::Field { base, .. } if let Some(&(key, ty)) = self.inp.record_fields.get(&id.0) => {
                let Some(obj) = self.word(*base)? else {
                    return Ok(Val::Never);
                };
                let key = self.b.ins_string(key).map_err(mir)?;
                Ok(Val::V(self.b.ins_heap_field_load(obj, Some(key), 0, ty).map_err(mir)?))
            }
            HirKind::Assign { place, value }
                if let HirKind::Field { base, .. } = self.hir.expr(*place).kind
                    && let Some(&(key, ty)) = self.inp.record_fields.get(&place.0) =>
            {
                // The value, the object, then the key, as `SetField`.
                let Some(v) = self.word(*value)? else {
                    return Ok(Val::Never);
                };
                if self.b.value_ty(v) != ty {
                    return Err("field type".into());
                }
                let Some(obj) = self.word(base)? else {
                    return Ok(Val::Never);
                };
                let key = self.b.ins_string(key).map_err(mir)?;
                self.b.ins_heap_field_store(obj, v, Some(key), None).map_err(mir)?;
                Ok(Val::Unit)
            }
            HirKind::Make { kind: MakeKind::Record(_), args } if let Some(words) = self.inp.records.get(&id.0) => {
                // Each value then its key, then `MakeDict`.
                let words = words.clone();
                let mut elems = Vec::with_capacity(2 * args.len());
                for (&arg, (key, ty)) in args.iter().zip(words) {
                    let Some(v) = self.word(arg)? else {
                        return Ok(Val::Never);
                    };
                    if self.b.value_ty(v) != ty {
                        return Err("record field type".into());
                    }
                    elems.push(v);
                    elems.push(self.b.ins_string(key).map_err(mir)?);
                }
                let obj = self.b.ins_alloc(MirAllocKind::Dict, elems).map_err(mir)?;
                Ok(Val::V(self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![obj]).map_err(mir)?))
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
            _ if lower::is_unit_make(self.hir, id) => Ok(Val::Unit),
            HirKind::Make { kind, args } if self.inp.pair_tys.contains_key(&id.0) => self.make_pair(id, kind, args),
            HirKind::Make { kind: MakeKind::Tuple, args } => self.make_tuple(args),
            HirKind::Make { kind: MakeKind::Array, args } if self.heap_array(id) => self.make_array(id, args),
            HirKind::Make { kind: MakeKind::Class(_), args } if self.inp.news.contains_key(&id.0) => self.make_object(id, args),
            HirKind::Make {
                kind: MakeKind::Variant { variant, .. },
                args,
            } if let Some(niche) = self.expr_ty(id).and_then(|t| (self.inp.niche)(t)) => self.make_niche(variant, args, niche),
            HirKind::Make {
                kind: MakeKind::Variant { enum_name, variant, tag: Some(tag), .. },
                args,
            } if let Some(tys) = self.expr_ty(id).and_then(|t| (self.inp.boxed_enum)(t, Some(variant))) => {
                self.make_boxed(enum_name, *tag, &tys, args)
            }
            // A scalar enum's variant is its backing constant.
            HirKind::Make {
                kind: MakeKind::Variant { enum_name, variant, .. },
                args,
            } if args.is_empty() && let Some(c) = (self.inp.scalar_variant)(enum_name, variant) => {
                Ok(Val::V(self.b.ins_const(c).map_err(mir)?))
            }
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
            // A boxed tuple's field: the same `Index`.
            HirKind::Index { base, index, kind: IndexKind::Tuple } => self.heap_index(id, *base, *index),
            HirKind::ForIn {
                pat,
                iterable,
                body,
                kind: Some(ForInKind::Array),
            } if self.heap_array(*iterable) => self.for_array(id, pat, *iterable, *body, None, Items::Array),
            HirKind::ForIn {
                pat,
                iterable,
                body,
                kind: Some(ForInKind::Dict),
            } => self.for_array(id, pat, *iterable, *body, None, Items::Dict),
            HirKind::ForIn {
                pat,
                iterable,
                body,
                kind: Some(ForInKind::Tuple { arity }),
            } => self.for_array(id, pat, *iterable, *body, None, Items::Tuple(*arity as usize)),
            HirKind::ForIn {
                pat: HirPat::Bind(local),
                iterable,
                body,
                kind: Some(ForInKind::Coroutine),
            } => self.for_coroutine(*local, *iterable, *body),
            HirKind::Match { scrutinee, arms } => self.match_(*scrutinee, arms, returned),
            // `let _ = e`: nothing can read `_`.
            HirKind::Let { local, init: Some(init) } if self.hir.local(*local).name == "_" => {
                Ok(if self.effect(*init)? { Val::Unit } else { Val::Never })
            }
            HirKind::Let { local, init } => {
                let init = init.ok_or("uninitialized let")?;
                self.refuse_array_copy(init)?;
                let ty = self.local_ty(*local)?;
                let slot = self.slot(*local)?;
                let Some(v) = (if ty == MirTy::HeapRef { self.boxed_word(init)? } else { self.word(init)? }) else {
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
                let Some(v) = (if ty == MirTy::HeapRef { self.boxed_word(*value)? } else { self.word(*value)? }) else {
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
            // A scalar's word roots nothing. A heap local's must be zeroed
            // (its register is scanned until overwritten), which direct MIR
            // cannot say: such a body keeps HIR emission's clearing stores.
            HirKind::Clear(locals)
                if locals.iter().all(|&l| {
                    !self.inp.stacks.contains_key(&l.0)
                        && !self.inp.sroa.contains_key(&l.0)
                        && !self.inp.pair_locals.contains_key(&l.0)
                        && self.local_ty(l).is_ok_and(|t| matches!(t, MirTy::I64 | MirTy::F64 | MirTy::Bool))
                }) =>
            {
                Ok(Val::Unit)
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
                self.for_range(local, *iterable, *body, *inclusive, *float, None)
            }
            // A user `into_iter` returning a range or array runs its loop.
            HirKind::ForIn {
                pat: HirPat::Bind(local),
                iterable,
                body,
                kind: Some(ForInKind::Custom { counted: Some(counted), .. }),
            } if let Some(&target) = self.inp.into_iters.get(&id.0) => match *counted {
                ForInCounted::Range { inclusive, float } => self.for_range(*local, *iterable, *body, inclusive, float, Some(target)),
                ForInCounted::Array => self.for_array(id, &HirPat::Bind(*local), *iterable, *body, Some(target), Items::Array),
                _ => Err("for in counted custom".into()),
            },
            HirKind::Return(value) => {
                match value {
                    None if self.unit_ret => self.ret_unit()?,
                    Some(v) if self.unit_ret => {
                        if !self.effect(*v)? {
                            return Ok(Val::Never);
                        }
                        self.ret_unit()?;
                    }
                    // A `()`-typed value (`return check(x)?`, a unit
                    // `match`) or a bare `return` in a one-word `Result<(),
                    // E>` function: the value for its effect, then `0`.
                    Some(v) if self.inp.ret_pair.is_none() && (lower::is_unit_make(self.hir, *v) || self.expr_ty(*v).is_some_and(is_unit)) => {
                        if !self.effect(*v)? {
                            return Ok(Val::Never);
                        }
                        self.ret_zero()?;
                    }
                    Some(v) => {
                        self.ret_tail = true;
                        match self.value(*v)? {
                            Val::Never => return Ok(Val::Never),
                            val => self.ret(val)?,
                        }
                    }
                    None if self.inp.ret_pair.is_none() => self.ret_zero()?,
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
    fn for_range(&mut self, local: HirLocal, iterable: HirId, body: HirId, inclusive: bool, float: bool, into_iter: Option<Label>) -> Result<Val, Refusal> {
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
            _ if let Some(target) = into_iter => {
                let Some(obj) = self.word(iterable)? else {
                    return Ok(Val::Never);
                };
                let abi = DenseAbi {
                    params: vec![self.b.value_ty(obj)],
                    ret: ty,
                    ret_hi: Some(ty),
                };
                match self.b.ins_call(target, vec![obj], &abi).map_err(mir)? {
                    (start, Some(end)) => (start, end),
                    _ => return Err("into_iter result".into()),
                }
            }
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
    /// `for x in h` over a coroutine, as HIR emission's: resume into `x`,
    /// stop once the handle is done (its completion value is never bound).
    fn for_coroutine(&mut self, local: HirLocal, iterable: HirId, body: HirId) -> Result<Val, Refusal> {
        let ty = self.local_ty(local)?;
        let Some(h) = self.word(iterable)? else {
            return Ok(Val::Never);
        };
        let head = self.b.create_block();
        let body_b = self.b.create_block();
        let exit = self.b.create_block();
        self.jump(head)?;
        self.switch(head);
        let x = self.b.ins_resume_coro(h, None, ty).map_err(mir)?;
        self.bind(local, x)?;
        let done = self.b.ins_done_coro(h).map_err(mir)?;
        self.b.branch(done, exit, body_b).map_err(mir)?;
        self.switch(body_b);
        self.loops.push(Loop {
            cont: head,
            cont_used: false,
            exit: Some(exit),
        });
        let live = self.effect(body);
        self.loops.pop();
        if live? {
            self.jump(head)?;
        }
        self.switch(exit);
        Ok(Val::Unit)
    }

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
        if let Some(out) = self.bitop_identity(id, op, lhs, rhs) {
            return out;
        }
        if let Some((x, n, shift)) = self.strength_reduce(op, lhs, rhs) {
            let Some(v) = self.word(x)? else {
                return Ok(Val::Never);
            };
            if self.expr_ty(id).and_then(scalar) != Some(self.b.value_ty(v)) {
                return Err("operator type".into());
            }
            let k = self.b.ins_const(MirConst::I64(i64::from(n))).map_err(mir)?;
            return Ok(Val::V(self.b.ins_binop(shift, v, k).map_err(mir)?));
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

    /// As HIR emission's `hir_strength_reduce`: `x / 2^n` as `x >> n` when
    /// `x` is a `byte` or proved non-negative, and `x * 2^n` as `x << n`
    /// when `x` is a `byte`. An int multiply stays: a shift would not trap
    /// on overflow.
    fn strength_reduce(&self, op: BinOp, lhs: HirId, rhs: HirId) -> Option<(HirId, u32, MirBinOp)> {
        let pow2 = |id: HirId| self.int_imm(id).and_then(crate::const_fold::strength_div_int);
        let byte = |id: HirId| self.expr_ty(id).is_some_and(lower::is_byte);
        let nonneg = self.hir.expr(lhs).flags.contains(HirFlags::NONNEG) || byte(lhs);
        match op {
            BinOp::IntMul => pow2(rhs)
                .filter(|_| byte(lhs))
                .map(|n| (lhs, n, MirBinOp::Shl))
                .or_else(|| pow2(lhs).filter(|_| byte(rhs)).map(|n| (rhs, n, MirBinOp::Shl))),
            BinOp::IntDiv if nonneg => pow2(rhs).map(|n| (lhs, n, MirBinOp::Shr)),
            _ => None,
        }
    }

    /// As HIR emission's `hir_bitop_identity`: a bitwise identity or
    /// annihilator lowers to its operand or a constant. Only a local or
    /// literal operand is dropped, so an effect still runs.
    fn bitop_identity(&mut self, id: HirId, op: BinOp, lhs: HirId, rhs: HirId) -> Option<Result<Val, Refusal>> {
        let trivial = |id: HirId| matches!(self.hir.expr(id).kind, HirKind::Local(_) | HirKind::Lit(Lit::Int(_) | Lit::Bool(_)));
        let same = matches!((&self.hir.expr(lhs).kind, &self.hir.expr(rhs).kind), (HirKind::Local(a), HirKind::Local(b)) if a == b);
        let operand = || match (self.int_imm(lhs), self.int_imm(rhs)) {
            (Some(k), None) if trivial(rhs) => Some((rhs, k)),
            (None, Some(k)) if trivial(lhs) => Some((lhs, k)),
            _ => None,
        };
        let out: Result<HirId, i64> = match op {
            BinOp::BitAnd | BinOp::BitOr if same => Ok(lhs),
            BinOp::BitXor if same => Err(0),
            BinOp::BitAnd => match operand()? {
                (_, 0) => Err(0),
                (x, -1) => Ok(x),
                _ => return None,
            },
            BinOp::BitOr => match operand()? {
                (x, 0) => Ok(x),
                (_, -1) => Err(-1),
                _ => return None,
            },
            BinOp::BitXor => match operand()? {
                (x, 0) => Ok(x),
                _ => return None,
            },
            BinOp::Shl | BinOp::Shr if self.int_imm(rhs) == Some(0) => Ok(lhs),
            _ => return None,
        };
        Some(match out {
            Ok(x) => self.value(x),
            Err(k) => {
                let c = match self.expr_ty(id).and_then(scalar)? {
                    MirTy::I64 => MirConst::I64(k),
                    MirTy::Bool => MirConst::Bool(k != 0),
                    _ => return None,
                };
                self.b.ins_const(c).map(Val::V).map_err(mir)
            }
        })
    }

    /// An integer literal, or a `const` local bound to one (HIR emission's
    /// `hir_int_imm`; `const` globals do not reach direct MIR).
    fn int_imm(&self, id: HirId) -> Option<i64> {
        match &self.hir.expr(id).kind {
            HirKind::Lit(Lit::Int(k)) => Some(*k),
            HirKind::Local(local) if self.hir.local(*local).kind == crate::hir::LocalKind::Const => {
                self.hir.exprs.iter().find_map(|e| match e.kind {
                    HirKind::Let { local: l, init: Some(init) } if l == *local => match self.hir.expr(init).kind {
                        HirKind::Lit(Lit::Int(k)) => Some(k),
                        _ => None,
                    },
                    _ => None,
                })
            }
            _ => None,
        }
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
        // A boxed two-word result kept as it comes back.
        let raw = std::mem::take(&mut self.raw_host);
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
            Some(_) if raw => (MirTy::HeapRef, None),
            _ if site.pair_ret => return Err("call type".into()),
            _ if unit => (MirTy::I64, None),
            _ => (self.inp.word(ty).ok_or_else(|| format!("call type {}", ty_kind(ty)))?, None),
        };
        let mut words = Vec::with_capacity(args.len());
        for (&arg, &pair) in args.iter().zip(&site.pair_args) {
            // A two-word type not passed as a pair goes boxed.
            let boxed = !pair && self.expr_ty(arg).is_some_and(|t| self.inp.word(t).is_none() && (self.inp.boxed_view)(t, None).is_some());
            if !pair && !boxed && self.expr_ty(arg).and_then(|t| self.inp.word(t)).is_none() {
                return Err("call argument type".into());
            }
            match (self.value(arg)?, pair) {
                (Val::Never, _) => return Ok(Val::Never),
                (Val::V(v), false) if boxed && self.b.value_ty(v) != MirTy::HeapRef => return Err("call argument boxed".into()),
                (Val::V(v), false) => words.push(v),
                (Val::Pair(lo, tag), false) if boxed => {
                    let ty = self.expr_ty(arg).ok_or("call argument type")?.clone();
                    match self.box_pair_val(&ty, lo, tag)? {
                        Val::V(v) => words.push(v),
                        _ => return Ok(Val::Never),
                    }
                }
                (Val::Pair(lo, hi), true) => words.extend([lo, hi]),
                // A boxed value the callee takes as a pair.
                (Val::V(v), true) if self.b.value_ty(v) == MirTy::HeapRef && let Some(&(lo_ty, hi_ty)) = self.inp.pair_tys.get(&arg.0) => {
                    let ty = self.expr_ty(arg).ok_or("call argument type")?.clone();
                    match self.unbox_word(&ty, v, lo_ty, hi_ty)? {
                        Val::Pair(lo, hi) => words.extend([lo, hi]),
                        _ => return Ok(Val::Never),
                    }
                }
                _ => return Err("call argument".into()),
            }
        }
        // `MakeCoro`: the arguments move into a new coroutine's frame.
        if site.coro {
            if ret != MirTy::HeapRef || ret_hi.is_some() {
                return Err("coroutine handle type".into());
            }
            let h = self.b.ins_alloc(MirAllocKind::Coro { target: site.target }, words).map_err(mir)?;
            return Ok(Val::V(self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![h]).map_err(mir)?));
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

    /// `resume h` / `resume h with v`: the coroutine runs to its next
    /// `yield`, whose word is the value.
    fn resume(&mut self, id: HirId, handle: HirId, value: Option<HirId>) -> Result<Val, Refusal> {
        let ty = self.expr_ty(id).ok_or("resume type")?;
        let unit = is_unit(ty);
        let dest_ty = if unit { MirTy::I64 } else { self.inp.word(ty).ok_or("resume type")? };
        let send = match value {
            Some(v) => match self.word(v)? {
                Some(w) => Some(w),
                None => return Ok(Val::Never),
            },
            None => None,
        };
        let Some(h) = self.word(handle)? else {
            return Ok(Val::Never);
        };
        let out = self.b.ins_resume_coro(h, send, dest_ty).map_err(mir)?;
        Ok(if unit { Val::Unit } else { Val::V(out) })
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
    /// The word a boxed tuple's field of type `ty` holds: a scalar or a
    /// plain heap word.
    fn tuple_item(&self, ty: &Ty) -> Option<MirTy> {
        scalar(ty).or_else(|| self.inp.plain_heap(ty).then_some(MirTy::HeapRef))
    }

    fn heap_array(&self, id: HirId) -> bool {
        self.expr_ty(id).is_some_and(|t| self.inp.word(t) == Some(MirTy::HeapRef) && self.inp.elem_word(t).is_some())
    }

    /// `xs[i]` on a heap array: the array, then the index, as HIR emission
    /// pushes them.
    fn heap_index(&mut self, id: HirId, base: HirId, index: HirId) -> Result<Val, Refusal> {
        let ty = match self.expr_ty(base) {
            // A tuple's field: a scalar.
            Some(t) if matches!(strip_readonly(t), Ty::Tuple(_)) => self.expr_ty(id).and_then(|t| self.tuple_item(t)),
            Some(t) => self.inp.elem_word(t),
            None => None,
        }
        .ok_or("element type")?;
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
        let ty = self.expr_ty(base).and_then(|t| self.inp.elem_word(t)).ok_or("element type")?;
        let v = if ty == MirTy::HeapRef { self.boxed_word(value)? } else { self.word(value)? };
        let Some(v) = v else {
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
    /// is the safepoint's, as the IL lift reads it. Heap elements are
    /// boxed words of an array of unknown element kind, as the lift builds.
    fn make_array(&mut self, id: HirId, args: &[HirId]) -> Result<Val, Refusal> {
        let ty = self
            .expr_ty(id)
            .and_then(|t| array_elem(t).and_then(scalar).or_else(|| self.inp.elem_word(t)))
            .ok_or("element type")?;
        let mut items = Vec::with_capacity(args.len());
        for &arg in args {
            let v = if ty == MirTy::HeapRef { self.boxed_word(arg)? } else { self.word(arg)? };
            let Some(v) = v else {
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

    /// A boxed user enum variant: `MakeEnum` of the payload words, pushed
    /// last field first (it pops field 0 first), as HIR emission's
    /// `hir_make`.
    fn make_boxed(&mut self, enum_name: &str, tag: u32, tys: &[Ty], args: &[HirId]) -> Result<Val, Refusal> {
        if !args.is_empty() && (self.inp.enum_drop)(enum_name) {
            return Err("make drop enum".into());
        }
        let mut words = Vec::with_capacity(args.len());
        for (&arg, t) in args.iter().zip(tys) {
            let want = self
                .inp
                .word(t)
                .filter(|_| !is_unit(t))
                .or_else(|| (self.inp.boxed_view)(t, None).map(|_| MirTy::HeapRef))
                .ok_or("make payload type")?;
            let Some(v) = self.word(arg)? else {
                return Ok(Val::Never);
            };
            if self.b.value_ty(v) != want {
                return Err("make payload word".into());
            }
            words.push(v);
        }
        words.reverse();
        let obj = self.b.ins_alloc(MirAllocKind::Enum { tag }, words).map_err(mir)?;
        Ok(Val::V(self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![obj]).map_err(mir)?))
    }

    /// A two-word enum value boxed to one word, as HIR emission's
    /// `emit_box_slots_to_enum`: the tag tested against each variant's but
    /// the last, then that variant made from the payload word.
    fn box_pair(&mut self, ty: &Ty, lo: ValueId, tag: ValueId) -> Result<Val, Refusal> {
        let (name, variants) = (self.inp.pair_variants)(ty).ok_or("box pair type")?;
        if (self.inp.enum_drop)(&name) {
            return Err("box drop enum".into());
        }
        let Some((&(last_tag, last_word), rest)) = variants.split_last() else {
            return Err("box pair variants".into());
        };
        // A heap payload rides the pair as an opaque word: the object
        // types it back, so the collector traces it.
        let make = |this: &mut Self, t: u32, word: Option<MirTy>| -> Result<ValueId, Refusal> {
            let words = match word {
                None => Vec::new(),
                Some(ty) => vec![this.fit(lo, ty)?],
            };
            let obj = this.b.ins_alloc(MirAllocKind::Enum { tag: t }, words).map_err(mir)?;
            this.b.ins_gc_barrier(MirGcKind::Safepoint, vec![obj]).map_err(mir)
        };
        let mut out = Vec::with_capacity(variants.len());
        for &(t, arity) in rest {
            let c = self.b.ins_const(MirConst::I64(i64::from(t))).map_err(mir)?;
            let hit = self.b.ins_cmp(MirCmpOp::Eq, tag, c).map_err(mir)?;
            let (take, next) = (self.b.create_block(), self.b.create_block());
            self.b.branch(hit, take, next).map_err(mir)?;
            self.switch(take);
            let obj = make(self, t, arity)?;
            out.push((self.cur, Val::V(obj)));
            self.switch(next);
        }
        let obj = make(self, last_tag, last_word)?;
        out.push((self.cur, Val::V(obj)));
        self.join_arms(out)
    }

    /// `v.push(x)` on a heap `Vec` of int/float: the Vec, then the value,
    /// then `ArrayPush` (a safepoint: it may grow the storage).
    fn push(&mut self, args: &[HirId]) -> Result<Val, Refusal> {
        let &[recv, value] = args else {
            return Err("push arity".into());
        };
        let ty = self.expr_ty(recv).and_then(|t| self.inp.elem_word(t));
        if !self.heap_array(recv) {
            return Err("push receiver".into());
        }
        let Some(arr) = self.word(recv)? else {
            return Ok(Val::Never);
        };
        let v = if ty == Some(MirTy::HeapRef) { self.boxed_word(value)? } else { self.word(value)? };
        let Some(v) = v else {
            return Ok(Val::Never);
        };
        if Some(self.b.value_ty(v)) != ty {
            return Err("push value type".into());
        }
        let out = self.b.ins_array_push(arr, v).map_err(mir)?;
        self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![out]).map_err(mir)?;
        Ok(Val::Unit)
    }

    /// `format(fmt, args)` with word arguments: `FORMAT` of the format string
    /// and each argument, a heap string the collector may see at once.
    fn format(&mut self, fmt: u32, args: &[HirId]) -> Result<Val, Refusal> {
        // The string first, as emission loads it, so the arguments land in
        // the registers after it.
        let fmt = self.b.ins_string(fmt).map_err(mir)?;
        let mut words = Vec::with_capacity(args.len());
        for &arg in args {
            let Some(v) = self.word(arg)? else {
                return Ok(Val::Never);
            };
            words.push(v);
        }
        let out = self.b.ins_format(fmt, words).map_err(mir)?;
        Ok(Val::V(self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![out]).map_err(mir)?))
    }

    /// A host native: the arguments in order, then `HostInvoke` (the native
    /// id is the instruction's, not a value the lowering has to find).
    fn host(&mut self, id: HirId, args: &[HirId]) -> Result<Val, Refusal> {
        let (native, layout, unbox) = &self.inp.hosts[&id.0];
        let (native, layout) = (*native, *layout);
        let raw = std::mem::take(&mut self.raw_host);
        let pair = match (self.inp.pair_tys.get(&id.0), unbox) {
            (Some(_), Some(_)) if raw => None,
            (Some(&tys), Some(variants)) => Some((tys, variants.clone())),
            (Some(_), None) => return Err("host pair result".into()),
            (None, _) => None,
        };
        let unit = self.expr_ty(id).is_some_and(is_unit);
        let ret = match self.expr_ty(id) {
            _ if pair.is_some() || raw => Some(MirTy::HeapRef),
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
        let out = self.b.ins_host_invoke_as(native, words, layout, ret).map_err(mir)?;
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
        // `ArrayLen` reads a string's byte length too.
        if !self.heap_array(arg) && !self.expr_ty(arg).is_some_and(is_string) {
            return Err("len operand".into());
        }
        let Some(arr) = self.word(arg)? else {
            return Ok(Val::Never);
        };
        Ok(Val::V(self.b.ins_array_len(arr).map_err(mir)?))
    }

    /// `for x in xs` over a heap array, as HIR emission lowers it: the
    /// length is read once, and the index steps after the body (or at a
    /// `continue`). A dict loops over its `(key, value)` entries array, a
    /// tuple over an array of its items. A tuple pattern binds each part
    /// from the item.
    fn for_array(&mut self, id: HirId, pat: &HirPat, iterable: HirId, body: HirId, into_iter: Option<Label>, items: Items) -> Result<Val, Refusal> {
        let it_ty = self.expr_ty(iterable).cloned();
        // A user `into_iter`'s array holds the loop's items.
        let elem = match (into_iter, items, &it_ty) {
            (Some(_), _, _) => match pat {
                HirPat::Bind(local) => scalar(self.hir.local(*local).ty.as_ref().ok_or("local type")?),
                _ => None,
            },
            (None, Items::Array, Some(t)) => self.inp.elem_word(t),
            (None, Items::Dict, _) => Some(MirTy::HeapRef),
            (None, Items::Tuple(_), Some(t)) => match strip_readonly(t) {
                Ty::Tuple(tys) => {
                    let words = tys.iter().map(|t| self.tuple_item(t)).collect::<Option<Vec<_>>>();
                    match words.as_deref() {
                        Some([w, rest @ ..]) if rest.iter().all(|r| r == w) => Some(*w),
                        _ => None,
                    }
                }
                _ => None,
            },
            _ => None,
        }
        .ok_or("for in element type")?;
        // Where the item goes: the bound local, or a temp the pattern's
        // parts read.
        let parts: Option<Vec<(HirLocal, MirTy)>> = match pat {
            HirPat::Bind(local) => {
                if self.local_ty(*local)? != elem {
                    return Err("for in element type".into());
                }
                None
            }
            HirPat::Tuple(pats) if elem == MirTy::HeapRef => {
                let mut out = Vec::new();
                for p in pats {
                    match p {
                        HirPat::Wild => {}
                        HirPat::Bind(l) => {
                            let ty = self.hir.local(*l).ty.as_ref().and_then(|t| self.tuple_item(t)).ok_or("for in pattern type")?;
                            if self.local_ty(*l)? != ty {
                                return Err("for in pattern type".into());
                            }
                            out.push((*l, ty));
                        }
                        _ => return Err("for in pattern".into()),
                    }
                }
                Some(out)
            }
            _ => return Err("for in pattern".into()),
        };
        let Some(src) = self.word(iterable)? else {
            return Ok(Val::Never);
        };
        let arr = match (into_iter, items) {
            (Some(target), _) => {
                let abi = DenseAbi {
                    params: vec![self.b.value_ty(src)],
                    ret: MirTy::HeapRef,
                    ret_hi: None,
                };
                self.b.ins_call(target, vec![src], &abi).map_err(mir)?.0
            }
            (None, Items::Array) => src,
            (None, Items::Dict) => {
                let obj = self.b.ins_alloc(MirAllocKind::DictEntries, vec![src]).map_err(mir)?;
                self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![obj]).map_err(mir)?
            }
            (None, Items::Tuple(arity)) => {
                let mut words = Vec::with_capacity(arity);
                for k in 0..arity {
                    let c = self.b.ins_const(MirConst::I64(k as i64)).map_err(mir)?;
                    words.push(self.b.ins_index(src, c, elem, false).map_err(mir)?);
                }
                let obj = self.b.ins_alloc(MirAllocKind::Array, words).map_err(mir)?;
                self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![obj]).map_err(mir)?
            }
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
        let pinned = matches!(items, Items::Array) && self.inp.pinned_for.contains(&id.0);
        let item = self.b.ins_index(arr, cur, elem, pinned).map_err(mir)?;
        match (pat, parts) {
            (HirPat::Bind(local), _) => {
                let x = self.slot(*local)?;
                self.b.def_local(x, item).map_err(mir)?;
            }
            (HirPat::Tuple(pats), Some(parts)) => {
                let mut parts = parts.into_iter();
                for (k, p) in pats.iter().enumerate() {
                    if let HirPat::Bind(_) = p {
                        let (l, ty) = parts.next().ok_or("for in pattern")?;
                        let c = self.b.ins_const(MirConst::I64(k as i64)).map_err(mir)?;
                        let v = self.b.ins_index(item, c, ty, false).map_err(mir)?;
                        let slot = self.slot(l)?;
                        self.b.def_local(slot, v).map_err(mir)?;
                    }
                }
            }
            _ => return Err("for in pattern".into()),
        }
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
                    // A `()` payload: `0`, after a `()`-typed value's effect.
                    [arg] if lower::is_unit_make(self.hir, *arg) || self.expr_ty(*arg).is_some_and(is_unit) => {
                        if !self.effect(*arg)? {
                            return Ok(Val::Never);
                        }
                        self.zero(lo_ty)?
                    }
                    [arg] => match self.word(*arg)? {
                        Some(v) if self.b.value_ty(v) == lo_ty => v,
                        Some(v) if lo_ty == MirTy::I64 && self.b.value_ty(v).is_heap_word() => self.fit(v, lo_ty)?,
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
            return self.match_nested(scrutinee, arms, tail);
        }
        if let Some(consts) = self.scalar_arms(scrutinee, arms) {
            return self.match_scalar(scrutinee, arms, &consts, tail);
        }
        let boxed_local = match self.hir.expr(scrutinee).kind {
            HirKind::Local(l) => self.boxed_local(l),
            // A class field holds a two-word enum boxed.
            HirKind::Field { .. } => self.inp.heap_fields.get(&scrutinee.0).is_some_and(|&(_, w)| w == MirTy::HeapRef),
            _ => false,
        };
        let boxed_host = self.host_pair(scrutinee) || self.boxed_call(scrutinee);
        if boxed_local || boxed_host || self.expr_ty(scrutinee).is_some_and(|t| (self.inp.boxed_enum)(t, None).is_some()) {
            return self.match_boxed(scrutinee, arms, tail);
        }
        if let Some(niche) = self.expr_ty(scrutinee).and_then(|t| (self.inp.niche)(t)) {
            return self.match_niche(scrutinee, arms, niche, tail);
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

    /// `match` on a boxed enum word, as HIR emission's boxed dispatch: a
    /// `JumpIfMatch` per arm but the last, which the fallthrough runs (and
    /// lays out first); a hit reads the variant's payload words.
    fn match_boxed(&mut self, scrutinee: HirId, arms: &[HirArm], tail: bool) -> Result<Val, Refusal> {
        let ty = self.expr_ty(scrutinee).ok_or("match scrutinee")?.clone();
        // A host's boxed enum result is matched as it comes back.
        self.raw_host = self.host_pair(scrutinee) || self.boxed_call(scrutinee);
        let obj = self.word(scrutinee);
        self.raw_host = false;
        let Some(obj) = obj? else {
            return Ok(Val::Never);
        };
        let last = arms.len() - 1;
        let mut hits = Vec::new();
        for arm in &arms[..last] {
            let HirPat::Variant { tag: Some(tag), .. } = arm.pat else {
                return Err("match arm pattern".into());
            };
            let (tys, binds) = self.variant_payload(&ty, &arm.pat)?;
            let mut words = Vec::with_capacity(tys.len());
            for (k, &t) in tys.iter().enumerate() {
                words.push(self.b.ins_match_payload(obj, k as u32, t).map_err(mir)?);
            }
            let (hit, miss) = (self.b.create_block(), self.b.create_block());
            self.b.jump_if_match(obj, tag, words.clone(), hit, miss).map_err(mir)?;
            hits.push((hit, arm, words, binds));
            self.switch(miss);
        }
        let mut out = Vec::new();
        let arm = &arms[last];
        match &arm.pat {
            HirPat::Variant { .. } => {
                let (tys, binds) = self.variant_payload(&ty, &arm.pat)?;
                // The fallthrough unpacks every payload word, or none.
                if binds.iter().any(Option::is_some) {
                    let mut words = Vec::with_capacity(tys.len());
                    for (k, &t) in tys.iter().enumerate() {
                        words.push(self.b.ins_match_payload(obj, k as u32, t).map_err(mir)?);
                    }
                    for (local, w) in binds.iter().zip(words) {
                        if let Some(local) = local {
                            self.bind(*local, w)?;
                        }
                    }
                }
            }
            HirPat::Bind(local) => self.bind(*local, obj)?,
            HirPat::Wild => {}
            _ => return Err("match arm pattern".into()),
        }
        let val = self.arm_value(arm.body, tail)?;
        if !matches!(val, Val::Never) {
            out.push((self.cur, val));
        }
        for (hit, arm, words, binds) in hits {
            self.switch(hit);
            for (local, w) in binds.iter().zip(words) {
                if let Some(local) = local {
                    self.bind(*local, w)?;
                }
            }
            let val = self.arm_value(arm.body, tail)?;
            if !matches!(val, Val::Never) {
                out.push((self.cur, val));
            }
        }
        self.join_arms(out)
    }

    /// A `match` whose patterns test inside a payload, as HIR emission's
    /// `hir_match_seq`: the scrutinee boxed, then each arm's tests in turn,
    /// a miss falling to the next arm. The last arm tests nothing: the
    /// checker proved the match exhaustive.
    fn match_nested(&mut self, scrutinee: HirId, arms: &[HirArm], tail: bool) -> Result<Val, Refusal> {
        let ty = self.expr_ty(scrutinee).ok_or("match scrutinee")?.clone();
        if (self.inp.boxed_view)(&ty, None).is_none() {
            return Err("nested match type".into());
        }
        let Some(obj) = self.boxed_word(scrutinee)? else {
            return Ok(Val::Never);
        };
        if self.b.value_ty(obj) != MirTy::HeapRef {
            return Err("nested match scrutinee".into());
        }
        if self.inp.match_tree
            && let Some(tree) = crate::hir::match_tree::outer_groups(arms)
            && tree.groups.iter().all(|g| (self.inp.scalar_variant)(g.enum_name, g.variant).is_none())
        {
            return self.match_grouped(&tree, obj, &ty, tail);
        }
        let last = arms.len() - 1;
        let mut out = Vec::new();
        for (i, arm) in arms.iter().enumerate() {
            let miss = (i < last).then(|| self.b.create_block());
            self.nested_test(&arm.pat, obj, &ty, miss)?;
            let val = self.arm_value(arm.body, tail)?;
            if !matches!(val, Val::Never) {
                out.push((self.cur, val));
            }
            if let Some(miss) = miss {
                self.switch(miss);
            }
        }
        self.join_arms(out)
    }

    /// A nested match as a decision tree on its outer tag, as HIR
    /// emission's `hir_match_grouped`: one `JumpIfMatch` per variant, then
    /// that variant's rows test only their sub-patterns. A group whose rows
    /// all miss runs the catch-all, emitted once.
    fn match_grouped(&mut self, tree: &crate::hir::match_tree::OuterTree<'_>, obj: ValueId, ty: &Ty, tail: bool) -> Result<Val, Refusal> {
        let catch = tree.catch_all.map(|_| self.b.create_block());
        let tested = if catch.is_some() { tree.groups.len() } else { tree.groups.len() - 1 };
        let mut groups = Vec::with_capacity(tree.groups.len());
        for (gi, g) in tree.groups.iter().enumerate() {
            let field_tys = (self.inp.boxed_view)(ty, Some(g.variant)).ok_or("nested variant payload")?;
            let mut words = Vec::with_capacity(field_tys.len());
            for (k, t) in field_tys.iter().enumerate() {
                let w = self.payload_word_ty(t)?;
                words.push(self.b.ins_match_payload(obj, k as u32, w).map_err(mir)?);
            }
            if gi < tested {
                let HirPat::Variant { tag: Some(tag), .. } = g.rows[0].1.pat else {
                    return Err("match arm pattern".into());
                };
                let (hit, miss) = (self.b.create_block(), self.b.create_block());
                self.b.jump_if_match(obj, tag, words.clone(), hit, miss).map_err(mir)?;
                groups.push((hit, field_tys, words));
                self.switch(miss);
            } else {
                // The last tag left just unpacks.
                groups.push((self.cur, field_tys, words));
            }
        }
        let mut out = Vec::new();
        if let (Some(catch), Some(arm)) = (catch, tree.catch_all) {
            self.jump(catch)?;
            self.switch(catch);
            self.nested_test(&arm.pat, obj, ty, None)?;
            let val = self.arm_value(arm.body, tail)?;
            if !matches!(val, Val::Never) {
                out.push((self.cur, val));
            }
        }
        for (g, (block, field_tys, words)) in tree.groups.iter().zip(groups) {
            self.switch(block);
            for (r, &(fields, arm)) in g.rows.iter().enumerate() {
                // With no catch-all, exhaustiveness covers this tag with its
                // rows: the last one needs no test.
                let last = r + 1 == g.rows.len();
                let miss = match catch {
                    None if last => None,
                    Some(c) if last => Some(c),
                    _ => Some(self.b.create_block()),
                };
                let subs = self.sub_patterns(g.enum_name, g.variant, fields);
                if !subs.is_empty() && subs.len() != field_tys.len() {
                    return Err("payload pattern".into());
                }
                for ((sub, &w), t) in subs.iter().zip(&words).zip(&field_tys) {
                    if let Some(sub) = sub {
                        if matches!(sub, HirPat::Variant { .. }) && (self.inp.niche)(t).is_some() {
                            return Err("nested niche pattern".into());
                        }
                        self.nested_test(sub, w, t, miss)?;
                    }
                }
                let val = self.arm_value(arm.body, tail)?;
                if !matches!(val, Val::Never) {
                    out.push((self.cur, val));
                }
                match miss {
                    Some(m) if !last => self.switch(m),
                    _ => {}
                }
            }
        }
        self.join_arms(out)
    }

    /// A variant pattern's sub-patterns in payload word order.
    fn sub_patterns<'p>(&self, enum_name: &str, variant: &str, fields: &'p HirPatFields) -> Vec<Option<&'p HirPat>> {
        match fields {
            HirPatFields::Unit => Vec::new(),
            HirPatFields::Tuple(parts) => parts.iter().map(Some).collect(),
            HirPatFields::Record(named) => (self.inp.payload_names)(enum_name, variant)
                .iter()
                .map(|name| named.iter().find(|(n, _)| n == name).map(|(_, p)| p))
                .collect(),
        }
    }

    /// The word a boxed variant's payload field of type `t` reads as.
    fn payload_word_ty(&self, t: &Ty) -> Result<MirTy, Refusal> {
        self.inp
            .word(t)
            .or_else(|| is_unit(t).then_some(MirTy::I64))
            .or_else(|| (self.inp.boxed_view)(t, None).map(|_| MirTy::HeapRef))
            .ok_or_else(|| "payload type".into())
    }

    /// Test `v` of type `ty` against `pat`, branching to `miss` when it
    /// fails (none: the pattern cannot fail), and bind its locals.
    fn nested_test(&mut self, pat: &HirPat, v: ValueId, ty: &Ty, miss: Option<BlockId>) -> Result<(), Refusal> {
        match pat {
            HirPat::Wild => Ok(()),
            HirPat::Bind(local) => {
                if self.hir.local(*local).ty.as_ref().is_some_and(is_unit) {
                    return Ok(());
                }
                self.bind(*local, v)
            }
            HirPat::Int(n) => {
                let Some(miss) = miss else {
                    return Ok(());
                };
                if self.b.value_ty(v) != MirTy::I64 {
                    return Err("nested int pattern".into());
                }
                let k = self.b.ins_const(MirConst::I64(*n)).map_err(mir)?;
                let hit = self.b.ins_cmp(MirCmpOp::Eq, v, k).map_err(mir)?;
                let next = self.b.create_block();
                self.b.branch(hit, next, miss).map_err(mir)?;
                self.switch(next);
                Ok(())
            }
            HirPat::Variant { enum_name, variant, tag, fields } => {
                if let Some(c) = (self.inp.scalar_variant)(enum_name, variant) {
                    let Some(miss) = miss else {
                        return Ok(());
                    };
                    if !matches!(c, MirConst::I64(_)) || self.b.value_ty(v) != MirTy::I64 {
                        return Err("nested scalar pattern".into());
                    }
                    let k = self.b.ins_const(c).map_err(mir)?;
                    let hit = self.b.ins_cmp(MirCmpOp::Eq, v, k).map_err(mir)?;
                    let next = self.b.create_block();
                    self.b.branch(hit, next, miss).map_err(mir)?;
                    self.switch(next);
                    return Ok(());
                }
                if self.b.value_ty(v) != MirTy::HeapRef {
                    return Err("nested variant word".into());
                }
                let field_tys = (self.inp.boxed_view)(ty, Some(variant)).ok_or("nested variant payload")?;
                let subs = self.sub_patterns(enum_name, variant, fields);
                if !subs.is_empty() && subs.len() != field_tys.len() {
                    return Err("payload pattern".into());
                }
                let mut words = Vec::with_capacity(field_tys.len());
                for (k, t) in field_tys.iter().enumerate() {
                    let w = self.payload_word_ty(t)?;
                    words.push(self.b.ins_match_payload(v, k as u32, w).map_err(mir)?);
                }
                if let Some(miss) = miss {
                    let tag = tag.ok_or("match arm pattern")?;
                    let hit = self.b.create_block();
                    self.b.jump_if_match(v, tag, words.clone(), hit, miss).map_err(mir)?;
                    self.switch(hit);
                }
                for ((sub, w), t) in subs.iter().zip(words).zip(&field_tys) {
                    if let Some(sub) = sub {
                        // A niche-word payload tests as its own layout; only
                        // boxed and scalar payloads are tested here.
                        if matches!(sub, HirPat::Variant { .. }) && (self.inp.niche)(t).is_some() {
                            return Err("nested niche pattern".into());
                        }
                        self.nested_test(sub, w, t, miss)?;
                    }
                }
                Ok(())
            }
            _ => Err("match arm pattern".into()),
        }
    }

    /// The variant a pointer-niche word carries as its payload, and the
    /// one it does not (the zero word, or `Err` with its low bit set).
    fn niche_sides(&self, scrutinee: HirId, niche: MirTy) -> (&'static str, &'static str) {
        let option = self.expr_ty(scrutinee).is_some_and(crate::typechecking::ty::is_option_ty);
        match niche {
            MirTy::NicheRes => ("Ok", "Err"),
            _ if option => ("Some", "None"),
            _ => ("Err", "Ok"),
        }
    }

    /// `match` on a pointer-niche word, as HIR emission's
    /// `hir_match_niche`: the payload side falls through and is laid out
    /// first; the zero side (or `Err`, its pointer untagged) is the jump.
    fn match_niche(&mut self, scrutinee: HirId, arms: &[HirArm], niche: MirTy, tail: bool) -> Result<Val, Refusal> {
        let (payload_side, other_side) = self.niche_sides(scrutinee, niche);
        let pick = |side: &str| {
            arms.iter().find(|a| match &a.pat {
                HirPat::Variant { variant, .. } => variant == side,
                _ => true,
            })
        };
        let first = pick(payload_side).ok_or("match arms")?;
        let second = pick(other_side).ok_or("match arms")?;
        let Some(w) = self.word(scrutinee)? else {
            return Ok(Val::Never);
        };
        let mut out = Vec::new();
        if std::ptr::eq(first, second) {
            self.niche_arm(first, w, false, tail, &mut out)?;
            return self.join_arms(out);
        }
        // The zero word (`LogNot`), or the `Err` bit.
        let other = if niche == MirTy::NicheRes {
            let one = self.b.ins_const(MirConst::I64(1)).map_err(mir)?;
            let bit = self.b.ins_binop(MirBinOp::BitAnd, w, one).map_err(mir)?;
            let zero = self.b.ins_const(MirConst::I64(0)).map_err(mir)?;
            self.b.ins_cmp(MirCmpOp::Ne, bit, zero).map_err(mir)?
        } else {
            self.b.ins_not(w).map_err(mir)?
        };
        let (hit, fall) = (self.b.create_block(), self.b.create_block());
        self.b.branch(other, hit, fall).map_err(mir)?;
        let start = self.order.len();
        self.switch(fall);
        self.niche_arm(first, w, false, tail, &mut out)?;
        if lower::cold_exit(self.hir, first.body) {
            self.mark_cold(start);
        }
        self.switch(hit);
        self.niche_arm(second, w, niche == MirTy::NicheRes, tail, &mut out)?;
        self.join_arms(out)
    }

    /// One arm of [`Self::match_niche`]: its binding reads the word itself
    /// (untagged first for `Err` of a niche `Result`).
    fn niche_arm(&mut self, arm: &HirArm, w: ValueId, untag: bool, tail: bool, out: &mut Vec<(BlockId, Val)>) -> Result<(), Refusal> {
        match &arm.pat {
            HirPat::Wild => {}
            HirPat::Bind(local) => self.bind(*local, w)?,
            HirPat::Variant { .. } => {
                let binds = lower::arm_fields(self.hir, &arm.pat).map_err(|e| e.to_string())?;
                let unit = |l: &HirLocal| self.hir.local(*l).ty.as_ref().is_some_and(is_unit);
                match binds.as_slice() {
                    [] | [None] => {}
                    [Some(local)] if unit(local) => {}
                    [Some(local)] => {
                        let payload = if untag {
                            let one = self.b.ins_const(MirConst::I64(1)).map_err(mir)?;
                            let x = self.b.ins_binop(MirBinOp::Xor, w, one).map_err(mir)?;
                            self.spec.extend([one, x]);
                            x
                        } else {
                            w
                        };
                        let cast = self.b.ins_cast(MirCastKind::Word, MirTy::HeapRef, payload).map_err(mir)?;
                        self.word_src.insert(cast, payload);
                        let payload = cast;
                        self.spec.insert(payload);
                        self.unniche.insert(payload, (w, untag));
                        self.bind(*local, payload)?;
                    }
                    _ => return Err("payload pattern".into()),
                }
            }
            _ => return Err("match arm pattern".into()),
        }
        let val = self.arm_value(arm.body, tail)?;
        if !matches!(val, Val::Never) {
            out.push((self.cur, val));
        }
        Ok(())
    }

    /// A variant of a pointer-niche type: the payload word, `Err` with its
    /// low bit set, or `0` for `None` / `Ok(())`.
    fn make_niche(&mut self, variant: &str, args: &[HirId], niche: MirTy) -> Result<Val, Refusal> {
        let zero = |this: &mut Self| -> Result<Val, Refusal> {
            let z = this.b.ins_const(MirConst::I64(0)).map_err(mir)?;
            Ok(Val::V(this.b.ins_cast(MirCastKind::Word, niche, z).map_err(mir)?))
        };
        let arg = match args {
            [] => return zero(self),
            // `Ok(e)` of a `()`-typed `e`: `e` for its effect, then `Ok(())`.
            [arg] if lower::is_unit_make(self.hir, *arg) || self.expr_ty(*arg).is_some_and(is_unit) => {
                if !self.effect(*arg)? {
                    return Ok(Val::Never);
                }
                if niche == MirTy::NicheRes {
                    // `Ok(())` carries the empty tuple object (`MakeTuple 0`).
                    let obj = self.b.ins_alloc(MirAllocKind::Tuple, Vec::new()).map_err(mir)?;
                    let obj = self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![obj]).map_err(mir)?;
                    return Ok(Val::V(self.b.ins_cast(MirCastKind::Word, niche, obj).map_err(mir)?));
                }
                return zero(self);
            }
            [arg] => *arg,
            _ => return Err("variant payload".into()),
        };
        let Some(v) = self.word(arg)? else {
            return Ok(Val::Never);
        };
        if !self.b.value_ty(v).is_heap_word() {
            return Err("niche payload type".into());
        }
        let err = niche == MirTy::NicheRes && variant == "Err";
        // `Err(e) => return Err(e)`: the matched word as it was.
        if let Some(&(w, untagged)) = self.unniche.get(&v)
            && untagged == err
            && self.b.value_ty(w) == niche
        {
            return Ok(Val::V(w));
        }
        if err {
            let one = self.b.ins_const(MirConst::I64(1)).map_err(mir)?;
            return Ok(Val::V(self.b.ins_binop(MirBinOp::BitOr, v, one).map_err(mir)?));
        }
        Ok(Val::V(self.b.ins_cast(MirCastKind::Word, niche, v).map_err(mir)?))
    }

    /// A boxed enum variant pattern's payload word types and the local each
    /// word binds (`None` for `_` or a `()` field).
    fn variant_payload(&self, ty: &Ty, pat: &HirPat) -> Result<(Vec<MirTy>, Vec<Option<HirLocal>>), Refusal> {
        let HirPat::Variant { variant, .. } = pat else {
            return Err("match arm pattern".into());
        };
        let fields = (self.inp.boxed_view)(ty, Some(variant)).ok_or("variant payload")?;
        let binds = lower::arm_fields(self.hir, pat).map_err(|e| e.to_string())?;
        if !binds.is_empty() && binds.len() != fields.len() {
            return Err("payload pattern".into());
        }
        let tys = fields
            .iter()
            .map(|t| {
                self.inp
                    .word(t)
                    .or_else(|| is_unit(t).then_some(MirTy::I64))
                    .or_else(|| (self.inp.boxed_view)(t, None).map(|_| MirTy::HeapRef))
            })
            .collect::<Option<Vec<_>>>()
            .ok_or("payload type")?;
        let binds = if binds.is_empty() { vec![None; tys.len()] } else { binds };
        Ok((tys, binds))
    }

    /// Each arm's constant when `match` tests an `int`, or a scalar enum
    /// backed by an `int` or `float`, against literals (`None` for a
    /// catch-all), as `lower::is_scalar_match`.
    fn scalar_arms(&self, scrutinee: HirId, arms: &[HirArm]) -> Option<Vec<Option<MirConst>>> {
        let ty = self.expr_ty(scrutinee)?;
        let int = matches!(strip_readonly(ty), Ty::Con(n) if n == crate::typechecking::ty::INT);
        if !int && (self.inp.scalar_enum)(ty).is_none() {
            return None;
        }
        arms.iter()
            .map(|arm| match &arm.pat {
                HirPat::Wild | HirPat::Bind(_) => Some(None),
                HirPat::Int(n) if int => Some(Some(MirConst::I64(*n))),
                HirPat::Variant {
                    enum_name, variant, fields, ..
                } if !int && unit_fields(fields) => (self.inp.scalar_variant)(enum_name, variant).map(Some),
                _ => None,
            })
            .collect()
    }

    /// `match` of a scalar word on constants: one equality test per arm but
    /// the last, or a binary search over many `int` literals, as HIR
    /// emission's `hir_match_scalar` / `hir_match_search`.
    fn match_scalar(&mut self, scrutinee: HirId, arms: &[HirArm], consts: &[Option<MirConst>], tail: bool) -> Result<Val, Refusal> {
        let Some(v) = self.word(scrutinee)? else {
            return Ok(Val::Never);
        };
        let literal = |pat: &HirPat| {
            let i = arms.iter().position(|a| std::ptr::eq(&a.pat, pat))?;
            match consts[i] {
                Some(MirConst::I64(n)) => Some(n),
                _ => None,
            }
        };
        let last = arms.len() - 1;
        let mut out = Vec::new();
        if let Some(plan) = crate::hir::match_tree::int_search(arms, literal) {
            let mut blocks: Vec<Option<BlockId>> = vec![None; arms.len()];
            for &(_, arm) in &plan.cases {
                blocks[arm].get_or_insert_with(|| self.b.create_block());
            }
            let default = *blocks[plan.default].get_or_insert_with(|| self.b.create_block());
            self.search_node(v, &plan.cases, &blocks, default)?;
            for (i, block) in blocks.into_iter().enumerate() {
                let Some(block) = block else { continue };
                self.switch(block);
                if let HirPat::Bind(local) = arms[i].pat {
                    self.bind(local, v)?;
                }
                let val = self.arm_value(arms[i].body, tail)?;
                if !matches!(val, Val::Never) {
                    out.push((self.cur, val));
                }
            }
            return self.join_arms(out);
        }
        let mut last_start = None;
        for (i, arm) in arms.iter().enumerate() {
            let miss = match consts[i] {
                Some(c) if i < last => {
                    let c = self.b.ins_const(c).map_err(mir)?;
                    let hit = self.b.ins_cmp(MirCmpOp::Eq, v, c).map_err(mir)?;
                    let arm_b = self.b.create_block();
                    let miss = self.b.create_block();
                    self.b.branch(hit, arm_b, miss).map_err(mir)?;
                    self.switch(arm_b);
                    Some(miss)
                }
                _ => None,
            };
            if let HirPat::Bind(local) = arm.pat {
                self.bind(local, v)?;
            }
            let val = self.arm_value(arm.body, tail)?;
            if !matches!(val, Val::Never) {
                out.push((self.cur, val));
            }
            if let Some(miss) = miss {
                last_start = Some(self.order.len());
                self.switch(miss);
            }
        }
        if let Some(start) = last_start
            && lower::cold_exit(self.hir, arms[last].body)
        {
            self.mark_cold(start);
        }
        self.join_arms(out)
    }

    /// Search the sorted `cases` for `v`, as `hir_search_node`: split on the
    /// middle literal with `<` (bytecode `LE`), and test a small leaf case by
    /// case.
    fn search_node(&mut self, v: ValueId, cases: &[(i64, usize)], blocks: &[Option<BlockId>], default: BlockId) -> Result<(), Refusal> {
        if cases.len() <= crate::hir::match_tree::SEARCH_LEAF {
            // `!=` falling through to the next test, so a hit is a taken
            // conditional jump straight to its arm.
            for &(n, arm) in cases {
                let c = self.b.ins_const(MirConst::I64(n)).map_err(mir)?;
                let miss = self.b.ins_cmp(MirCmpOp::Ne, v, c).map_err(mir)?;
                let next = self.b.create_block();
                self.b.branch(miss, next, blocks[arm].expect("case arm has a block")).map_err(mir)?;
                self.switch(next);
            }
            self.b.jump(default).map_err(mir)?;
            return Ok(());
        }
        let mid = cases.len() / 2;
        let c = self.b.ins_const(MirConst::I64(cases[mid].0)).map_err(mir)?;
        let below = self.b.ins_cmp(MirCmpOp::Lt, v, c).map_err(mir)?;
        let lower = self.b.create_block();
        let upper = self.b.create_block();
        self.b.branch(below, lower, upper).map_err(mir)?;
        self.switch(lower);
        self.search_node(v, &cases[..mid], blocks, default)?;
        self.switch(upper);
        self.search_node(v, &cases[mid..], blocks, default)
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
        // A heap payload rides the pair as an opaque `i64`.
        let ty = self.local_ty(bound)?;
        let payload = if self.b.value_ty(payload) == MirTy::I64 && ty.is_heap_word() {
            self.fit(payload, ty)?
        } else {
            payload
        };
        self.bind(bound, payload)
    }

    /// The value where the arms reaching the current block meet: one φ per
    /// word.
    fn join(&mut self, arms: Vec<(BlockId, Val)>) -> Result<Val, Refusal> {
        if arms.iter().all(|(_, v)| matches!(v, Val::Unit)) {
            return Ok(Val::Unit);
        }
        // One arm reaches the join: its value dominates it.
        if let [(_, val)] = arms.as_slice() {
            return Ok(*val);
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
        if els.is_none() && !tail {
            return self.if_then(c, negated, then_b, then);
        }
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

    /// A statement `if` without an `else`: the false edge goes straight to
    /// the join, so no empty block sits in front of it.
    fn if_then(&mut self, c: ValueId, negated: bool, then_b: BlockId, then: HirId) -> Result<Val, Refusal> {
        let join = self.b.create_block();
        if negated {
            self.b.branch(c, join, then_b).map_err(mir)?;
        } else {
            self.b.branch(c, then_b, join).map_err(mir)?;
        }
        let start = self.order.len();
        self.switch(then_b);
        if matches!(self.arm_value(then, false)?, Val::Never) {
            if lower::cold_exit(self.hir, then) {
                self.mark_cold(start);
            }
        } else {
            self.jump(join)?;
        }
        self.switch(join);
        Ok(Val::Unit)
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
        HirKind::ForIn { kind, pat, .. } => match kind {
            Some(ForInKind::Array) if !matches!(pat, HirPat::Bind(_)) => "for in array pattern",
            Some(ForInKind::Array) => "for in array",
            Some(ForInKind::Tuple { .. }) => "for in tuple",
            Some(ForInKind::Dict) => "for in dict",
            Some(ForInKind::Coroutine) => "for in coroutine",
            Some(ForInKind::Custom { counted: Some(_), .. }) => "for in counted custom",
            Some(ForInKind::Custom { .. }) => "for in iterator",
            _ => "for in",
        },
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
