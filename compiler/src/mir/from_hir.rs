//! Lower a planned HIR body straight to MIR: every body every engine runs
//! comes from here.
//!
//! The types come from the checker, locals are read and written by name,
//! and control flow is built from `if` / `loop` / `break` directly. The
//! plan (`codegen/emit_hir.rs`) decides each value's layout, each call's
//! target and ABI, and which locals live in frame slots (two-word pairs,
//! stack arrays, class locals one slot per field). Lowering asks for those
//! through [`LowerCx`] as it reaches each node: a fact about the plan, or
//! the node's site ([`HirSite`], `codegen/emit_hir_mir.rs`), which interns
//! its strings and requests the clones and adapters its code calls.
//! A construct this does not take refuses with its name; the body then
//! has no MIR, which is a compile error if anything reaches it.
//!
//! Each local's [`LocalId`] is the frame slot HIR lowering gave it, so the
//! sidecars keyed by slot (debug locals) read the same numbers.

use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};

use common::DebugLoc;

use crate::hir::{classify, query, BinOp, Builtin, Callee, HirArm, HirBody, HirFlags, HirId, HirKind, HirPat, HirPatFields, IndexKind, Lit, LocalId as HirLocal, MakeKind, UnOp};
use crate::bodies::Label;
use crate::typechecking::infer::{ForInCounted, ForInKind};
use crate::layout::ValueLayout;
use crate::typechecking::ty::{strip_readonly, vec_element_ty, Ty};

use crate::codegen::BodyFrame;
use crate::layout::CallAbi;
use super::builder::{MirBuilder, MirError};
use super::func::MirFunc;
use super::inst::{BlockId, LocalId, MirAllocKind, MirBinOp, MirCastKind, MirCmpOp, MirConst, MirGcKind, MirInst, MirLaneShape, Terminator, ValueId};
use super::ty::MirTy;

/// [`Lower::unbox_pair`]'s arity of a variant whose payload is `()`: its
/// box holds one word, which the pair carries as `0`.
const UNIT_PAYLOAD: u32 = u32::MAX;

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

/// How a `%v` argument of a planned `format` call becomes its text.
#[derive(Clone, Debug)]
pub enum HirShow {
    /// A one-word `CALL` of the value's `show` entry: on the value itself
    /// for a raw entry, else on its word (`word`, a `float` / `bool` as
    /// its bits), then the instance's dictionary when the entry takes one
    /// (`dicts`: a bounded generic instance's shared entry, #2479).
    Call { target: Label, word: bool, dicts: Vec<HirDictOp> },
    /// `STRINGIFY` (a type with no `Show` instance).
    Stringify,
    /// A bound type parameter's `show` through one of the body's
    /// dictionary parameters (by frame slot): the code pointer in its
    /// method slot, `CallIndirect` on the value's word and the dictionary.
    Bound { dict: u32, method: u32 },
    /// A boxed tuple or a record: each part read from its slot and shown,
    /// then `FORMAT` of the format string (by string table index) on the
    /// texts. A record's slots are its fields sorted by name; a tuple's
    /// (`tuple`) never change.
    Product { fmt: u32, parts: Vec<(Ty, HirShow)>, tuple: bool },
}

/// A call site HIR emission lowered to a plain `CALL`.
#[derive(Clone, Debug)]
pub struct HirCallSite {
    pub target: Label,
    /// Whether each argument goes as two words.
    pub pair_args: Vec<bool>,
    /// Whether the result is two words.
    pub pair_ret: bool,
    /// A `gen fn` call: `MakeCoro`, its result the handle.
    pub coro: bool,
    /// The result is a niche `Option` / `Result` the callee returns boxed.
    pub boxed_ret: bool,
    /// The call site's instance dictionaries, passed after the arguments.
    pub dicts: Vec<HirDictOp>,
    /// Each argument a shared generic takes as a bare type parameter,
    /// passed as its word (empty: none).
    pub words: Vec<bool>,
    /// A shared generic returns a bare type parameter: the result is read
    /// back from its word.
    pub word_ret: bool,
    /// Each argument's closure adapter, where the callee calls the
    /// function value with boxed arguments: `MakeFn` of the adapter's
    /// entry (the label, then the operand) over the value (empty: none).
    pub adapt: Vec<Option<(Label, u32)>>,
    /// The closure result's adapter, where a shared generic body returns
    /// it in its boxed ABI: `MakeFn` of the adapter's entry (the label,
    /// then the operand) over the result (#1532).
    pub ret_adapt: Option<(Label, u32)>,
    /// The result's items a shared generic body holds boxed (a bare type
    /// parameter's value inside the `Option` or tuple it returns),
    /// unboxed into a rebuilt object (#1549).
    pub ret_items: Option<HirRetItems>,
}

/// How a shared generic body's result object is rebuilt with the items
/// holding its bare type parameters' values unboxed (#1549): the shared
/// body keeps such a value boxed inside the object, a ground caller reads
/// the item in its own layout.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum HirRetItems {
    /// A tuple: each item, and the rebuilt tuple's word kinds.
    Tuple { items: Vec<HirRetItem>, kinds: u8 },
    /// An enum: each variant to rebuild (its tag, payload items and word
    /// kinds); any other variant is kept as it is.
    Enum { variants: Vec<(u32, Vec<HirRetItem>, u8)> },
}

/// One item of a [`HirRetItems`] object: its ground word, and whether the
/// shared body holds it boxed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HirRetItem {
    pub ty: MirTy,
    pub unbox: bool,
}

/// A trait method or operator on a bound type parameter in a shared
/// generic body, as HIR emission's `HirBuiltin::Bound` / `HirOp::Bound`:
/// each operand's word, the dictionary, then the code pointer in its
/// method slot through `CallIndirect`.
#[derive(Clone, Debug)]
pub struct HirBoundCall {
    /// Frame slot of the dictionary: one of the body's dictionary
    /// parameters.
    pub dict: u32,
    /// The method's slot in the dictionary.
    pub method: u32,
    /// The result is a ground niche `Option` / `Result` the dictionary's
    /// adapter returns boxed (the trait types it with a type variable).
    pub boxed_ret: bool,
    /// The dictionaries of the method's own bounds (`fn third<W: Show>`),
    /// passed after the bound's (#1471).
    pub dicts: Vec<HirDictOp>,
    /// A dictionary forwarder's call (#2476), `(index, own)`: the
    /// dictionary is no frame slot but the argument before the last `own`,
    /// and the call goes to element `index` of it, a superclass's
    /// dictionary: that one's `method` slot, called with that one.
    pub forward: Option<(u32, u32)>,
}

/// One step of building instance dictionaries, as codegen plans them
/// (`DictStep`, `plan_instance_dict`): a stack program whose words left
/// are the dictionaries.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum HirDictOp {
    /// `CodePtr` of a method's entry.
    Code(Label),
    /// `Const 0`: evidence a `PolyFn` value's call fills in.
    Null,
    /// `MakeTuple` of the top `n` words: a dictionary.
    Tuple(u32),
    /// The top value as its word: an existential pack's value.
    Word,
    /// `Load` of frame slot `n`, one of the body's own dictionary
    /// parameters: a dictionary it forwards.
    Load(u32),
    /// A ground dictionary: `ops` (only `Code`, `Kinds` and `Tuple`) build it once
    /// into static `slot`; later runs load it from there.
    Interned { slot: u32, ops: Vec<HirDictOp> },
    /// `Const`: a shared generic entry's `kinds` word, its bits known at
    /// the call (#2506).
    Kinds(u64),
    /// The top `kinds` word with bit `to` set where the body's own `kinds`
    /// word has bit `from`: a type parameter the call forwards.
    KindsBit { from: u8, to: u8 },
    /// The body's own `kinds` word: what a lambda of it captures.
    KindsOwn,
}

/// How a user iterator's `next` returns `Option<Item>`, as HIR emission
/// called it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HirIterNext {
    /// One niche word: zero is `None`, else the item.
    Niche,
    /// `[payload, tag]`: tag zero is `None`.
    Pair,
    /// A boxed `Option`: `Some` (this tag) carries the item.
    Boxed { some: u32 },
}

/// A `for x in it` over a user iterator, as HIR emission emitted it:
/// `into_iter` once, then `next` until it returns `None`.
#[derive(Clone, Debug)]
pub struct HirIterLoop {
    /// `None` when the iterable is the iterator.
    pub into_iter: Option<Label>,
    pub next: Label,
    /// The iterator `into_iter` returns.
    pub iter_ty: Ty,
    /// The instance dictionaries `into_iter` and `next` take after the
    /// receiver, where they take one (a generic-head instance's method,
    /// which reads its `kinds` word there, #2506).
    pub into_iter_dicts: Vec<HirDictOp>,
    pub next_dicts: Vec<HirDictOp>,
    pub shape: HirIterNext,
}

/// A lane array's element: its two words' types and how it is boxed as
/// one word ([`MirLaneShape`], whose `tag_mask` is the tags under which
/// the first word is a heap reference).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HirLaneElem {
    pub lo: MirTy,
    pub hi: MirTy,
    pub shape: MirLaneShape,
}

/// A `Vec` method call that works on a lane array's lanes
/// ([`HirLaneUses::calls`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HirLaneCall {
    /// `v.pop()`: the last element's words, taken out of the array.
    Pop,
    /// `v.remove(i)`: element `i`'s words, the rest moved down one.
    Remove,
    /// `v.insert(i, x)`: the elements from `i` moved up one, `x` at `i`.
    Insert,
    /// A thunk whose native takes either array form (`clear`, `reserve`,
    /// `capacity`, `from`): called as it is, the array kept in lanes.
    Thunk,
    /// `Vec::new()` of a lane type: an empty lane array.
    New,
    /// `Vec::with_capacity(n)` of a lane type: an empty lane array with
    /// room for `n`.
    WithCapacity,
}

impl HirLaneCall {
    /// Whether the call takes its receiver (argument 0) as a lane array.
    pub fn takes_lanes(self) -> bool {
        !matches!(self, Self::New | Self::WithCapacity)
    }
}

/// A function value HIR emission built: `MakeFn` of a lambda's or a named
/// function's code (`kind` [`MirAllocKind::Fn`], each capture read from its
/// local), or `MakePolyFn` of a generic function's (`MakePolyFnCapture`
/// over its instance dictionaries, `dicts`, when it has bounds). On a
/// call, a partial application: `MakeFn` of the callee over the call's
/// arguments.
#[derive(Clone, Debug)]
pub struct HirFnValue {
    pub kind: MirAllocKind,
    pub captures: Vec<HirLocal>,
    /// Per capture: whether the lambda takes it as its two words (a
    /// two-slot local's), not one.
    pub pairs: Vec<bool>,
    pub dicts: Vec<HirDictOp>,
    /// Captures taken as a function value made here instead of their
    /// local's word: a local bound from a generic function, made at the
    /// type the lambda holds it at (#2658).
    pub remade: Vec<(usize, HirFnValue)>,
}

/// A call through a `PolyFn` local as HIR emission made it: each argument
/// a bare type parameter takes passed as its word (`words`), the call
/// site's instance dictionaries, then `CallIndirect`, then the result read
/// back from its word when the function returns a bare type parameter.
#[derive(Clone, Debug)]
pub struct HirPolyCall {
    pub words: Vec<bool>,
    pub dicts: Vec<HirDictOp>,
    pub word_ret: bool,
    /// The closure result's adapter from the shared body's boxed ABI, as
    /// [`HirCallSite::ret_adapt`] (#1589).
    pub ret_adapt: Option<(Label, u32)>,
    /// A `forall` parameter's result holding its type parameters, read
    /// from the shared body's layout (#2723): an open `Option` / `Result`
    /// at a niche type comes back as its enum object, as
    /// [`HirCallSite::boxed_ret`]; a tuple or enum holding a bare
    /// parameter's value is rebuilt, as [`HirCallSite::ret_items`].
    pub boxed_ret: bool,
    pub ret_items: Option<HirRetItems>,
}

/// A user operator HIR emission lowered to its trait instance method's
/// plain `CALL`, each operand boxed as the instance takes it.
#[derive(Clone, Debug)]
pub struct HirOpCall {
    pub site: HirCallSite,
    /// A shift count passes as its plain `int` (#883).
    pub shift: bool,
}

/// An auto-par call site planning sent to the callee's fork-join
/// worker: the worker called on the arguments and the
/// `hops` budget; with `seq`, a one-argument site whose argument is below
/// the cutoff calls the sequential function instead.
#[derive(Clone, Debug)]
pub struct HirParFork {
    pub worker: Label,
    pub hops: i64,
    /// The sequential function and the cutoff below which it is called.
    pub seq: Option<(Label, i64)>,
}

/// A parallel loop planning moved into its own fork-join function:
/// the loop is a plain `CALL` of it, which takes the
/// accumulator, the live captures and the runtime bounds and returns the
/// new accumulator.
#[derive(Clone, Debug)]
pub struct HirParLoop {
    pub target: Label,
    /// The `int` locals passed, in order.
    pub args: Vec<HirLocal>,
    /// The accumulator the result is stored to.
    pub acc: HirLocal,
    /// The loop's index local and its value after the loop, when the loop
    /// leaves it set.
    pub index: Option<(HirLocal, HirParIndex)>,
}

/// A parallel loop's index after it: a const range's last value, or a
/// dynamic range's first value at or past its end, counting from its start
/// by the stride (the start itself when the range is empty).
#[derive(Clone, Copy, Debug)]
pub enum HirParIndex {
    Const(i64),
    Past { begin: HirParBound, end: HirParBound, stride: i64 },
}

/// A dynamic parallel loop's bound: a constant, or an `int` local plus a
/// bias (`1` for an inclusive end).
#[derive(Clone, Copy, Debug)]
pub enum HirParBound {
    Const(i64),
    Local(HirLocal, i64),
}

impl Lower<'_> {
    /// The MIR type of a one-word value: [`word`], a class object or a
    /// scalar enum's backing word.
    fn word_ty(&self, ty: &Ty) -> Option<MirTy> {
        // `Matrix<D>` is its data at run time.
        if let Some(data) = crate::typechecking::aggregate_arith::unwrap_matrix_ty(strip_readonly(ty)) {
            return self.word_ty(data);
        }
        // An array of arrays is a heap word when its innermost array is:
        // no class, enum or niche is an array. Asked of the innermost one
        // only, rather than of every array around it: each level's checks
        // walk the whole type, so a value `d` arrays deep took `O(d^2)` per
        // node (#2657).
        if matches!(strip_readonly(ty), Ty::Array { .. } | Ty::App(..))
            && let Some(inner) = array_elem(ty).filter(|e| array_elem(e).is_some())
        {
            let mut innermost = inner;
            while let Some(e) = array_elem(innermost).filter(|e| array_elem(e).is_some()) {
                innermost = e;
            }
            if self.word_ty(innermost) == Some(MirTy::HeapRef) {
                return Some(MirTy::HeapRef);
            }
        }
        word(ty)
            .or_else(|| (self.cx.is_class(ty) || opaque_heap(ty) || is_gc_handle(ty)).then_some(MirTy::HeapRef))
            .or_else(|| self.cx.scalar_enum(ty))
            .or_else(|| self.cx.niche(ty))
            .or_else(|| self.cx.boxed_enum(ty, None).map(|_| MirTy::HeapRef))
            .or_else(|| {
                array_elem(ty)
                    .and_then(|e| self.heap_elem(e).or_else(|| self.cx.niche(e)).or_else(|| self.cx.scalar_enum(e)))
                    .map(|_| MirTy::HeapRef)
            })
            .or_else(|| (self.opaque_vars && is_var(ty)).then_some(MirTy::I64))
            .or_else(|| (self.opaque_vars && array_elem(ty).is_some_and(is_var)).then_some(MirTy::HeapRef))
            // A higher-kinded `P<A>` (a type variable applied): an object
            // of whatever type constructor `P` is, which the body only
            // moves.
            .or_else(|| (self.opaque_vars && is_var_app(ty)).then_some(MirTy::HeapRef))
            // A bare-class existential: its `[boxed value, dictionary]` pack.
            .or_else(|| matches!(strip_readonly(ty), Ty::Existential { .. }).then_some(MirTy::HeapRef))
            // A foreign `ptr`: a raw address, never a heap word.
            .or_else(|| (matches!(strip_readonly(ty), Ty::Con(n) if n == "ptr") && !self.cx.is_class(ty)).then_some(MirTy::I64))
    }

    /// An array element type held as a heap word: a string, class, array,
    /// boxed enum or boxed tuple.
    fn heap_elem(&self, elem: &Ty) -> Option<MirTy> {
        // A `()` element is the empty tuple object.
        let heap = self.word_ty(elem) == Some(MirTy::HeapRef)
            || is_unit(elem)
            || matches!(strip_readonly(elem), Ty::Tuple(items) if !items.is_empty())
            || self.cx.boxed_view(elem, None).is_some();
        heap.then_some(MirTy::HeapRef)
    }

    /// A heap word stored in a tuple or array slot as the value itself:
    /// not a niche word (stored boxed).
    fn plain_heap(&self, ty: &Ty) -> bool {
        (self.word_ty(ty) == Some(MirTy::HeapRef) && self.cx.niche(ty).is_none())
            || matches!(strip_readonly(ty), Ty::Tuple(items) if !items.is_empty())
    }

    /// The word type of an array's elements that direct MIR reads and
    /// writes: a scalar (a packed `byte` / `bool` element is read and
    /// written as its word), a niche `Option` / `Result` word (stored as
    /// the word itself, as HIR emission stores it), or a heap word.
    fn elem_word(&self, arr: &Ty) -> Option<MirTy> {
        let elem = array_elem(arr)?;
        // A scalar enum's element is its backing word (#1903).
        match scalar(elem).or_else(|| self.cx.scalar_enum(elem)) {
            Some(t) => Some(t),
            // A type parameter's element: the opaque word as stored.
            _ if self.opaque_vars && is_var(elem) => Some(MirTy::I64),
            _ => self.cx.niche(elem).or_else(|| self.heap_elem(elem)),
        }
    }

    /// The word types of `id`'s value when it is a two-word pair.
    fn pair_tys(&self, id: HirId) -> Option<(MirTy, MirTy)> {
        let cell = self.facts.pair_tys.get(id.0 as usize)?;
        *cell.get_or_init(|| self.expr_ty(id).and_then(|t| self.cx.pair_tys(t)))
    }

    /// A two-word value `id` (a call's result) whose low word may carry a
    /// heap payload ([`MirFunc::heap_pair_words`]).
    fn heap_pair(&self, id: HirId) -> bool {
        self.pair_tys(id).is_some() && self.expr_ty(id).is_some_and(|t| self.cx.pair_holds_heap(t))
    }

    /// A pair local's second-word slot and word types.
    fn pair_local_tys(&self, local: HirLocal) -> Option<(u32, MirTy, MirTy)> {
        let cell = self.facts.pair_locals.get(local.0 as usize)?;
        // A pair local the body never binds (its `let` folded away by
        // inlining) has no second word: nothing reads it.
        *cell.get_or_init(|| {
            let (lo, hi) = self.cx.pair_local(local.0)?;
            Some((*self.frame.tags.get(&local.0)?, lo, hi))
        })
    }

    /// `id`'s element when it is a lane array (`Vec` of a two-word type).
    fn lane(&self, id: HirId) -> Option<HirLaneElem> {
        let cell = self.facts.lanes.get(id.0 as usize)?;
        *cell.get_or_init(|| self.expr_ty(id).and_then(|t| self.cx.lane_elem(t)))
    }

    /// A `Vec` method call on a lane array, or one making a lane array,
    /// that works on the lanes.
    fn lane_call_of(&self, id: HirId) -> Option<HirLaneCall> {
        let HirKind::Call { args, .. } = &self.hir.expr(id).kind else {
            return None;
        };
        if self.lane(id).is_none() && !args.first().is_some_and(|&a| self.lane(a).is_some()) {
            return None;
        }
        self.facts.lane_uses.get_or_init(|| self.cx.lane_uses()).calls.get(&id.0).copied()
    }

    /// Lane array `id` goes on to code that reads it one word per element.
    fn lane_escape(&self, id: HirId) -> bool {
        self.lane(id).is_some() && self.facts.lane_uses.get_or_init(|| self.cx.lane_uses()).escapes.contains(&id.0)
    }

    /// How field read or write `id` reaches the field.
    fn field(&self, id: HirId) -> HirFieldFact {
        let Some(cell) = self.facts.fields.get(id.0 as usize) else {
            return HirFieldFact::default();
        };
        *cell.get_or_init(|| self.cx.field(id, self.frame_field(id).is_some()))
    }

    /// A heap object's field `id` that holds the word its type says (or an
    /// open niche field's boxed enum object): its slot and word type.
    fn heap_field(&self, id: HirId) -> Option<(u32, MirTy)> {
        match self.field(id).word {
            HirFieldWord::Heap { slot, ty } => Some((slot, ty)),
            HirFieldWord::OpenNiche { slot, .. } => Some((slot, MirTy::HeapRef)),
            _ => None,
        }
    }

    /// A flat two-word class field `id`: its slot, its tail slot and their
    /// word types.
    fn pair_field(&self, id: HirId) -> Option<(u32, u32, MirTy, MirTy)> {
        match self.field(id).word {
            HirFieldWord::Pair { slot, tail, lo, hi } => Some((slot, tail, lo, hi)),
            _ => None,
        }
    }

    /// A stack array local's first slot, length and element type.
    fn stack(&self, local: u32) -> Option<(u32, usize, MirTy)> {
        let cell = self.facts.stacks.get(local as usize)?;
        *cell.get_or_init(|| {
            let (n, elem) = self.cx.stack(local)?;
            Some((self.frame.slot(HirLocal(local))?, n, elem))
        })
    }

    /// The length of `id` when it is a stack array local, as
    /// [`query::aggregate_src`] reads it.
    fn stack_lens(&self, id: HirId) -> HashMap<u32, usize> {
        match self.hir.expr(id).kind {
            HirKind::Local(local) => self.stack(local.0).map(|(_, n, _)| (local.0, n)).into_iter().collect(),
            _ => HashMap::new(),
        }
    }

    /// A class local kept in frame slots: each field slot's type.
    fn sroa(&self, local: u32) -> Option<&Vec<MirTy>> {
        let cell = self.facts.sroa.get(local as usize)?;
        cell.get_or_init(|| self.cx.sroa(local)).as_ref()
    }

    /// The class of an `sroa` local: its type id and field count.
    fn sroa_class(&self, local: u32) -> Option<(u32, u32)> {
        self.sroa(local)?;
        self.cx.sroa_class(local)
    }

    /// The frame slot of field read or write `id` on an `sroa` local.
    fn frame_field(&self, id: HirId) -> Option<u32> {
        let cell = self.facts.frame_fields.get(id.0 as usize)?;
        *cell.get_or_init(|| {
            let (local, field) = self.cx.frame_field(id)?;
            Some(self.frame.slot(HirLocal(local))? + field)
        })
    }

    /// What `new` `id` makes.
    fn new_fact(&self, id: HirId) -> &HirNew {
        static NONE: HirNew = HirNew { object: None, tails: Vec::new(), arg_niches: Vec::new() };
        match self.facts.news.get(id.0 as usize) {
            Some(cell) => cell.get_or_init(|| self.cx.new_object(id)),
            None => &NONE,
        }
    }

    /// `new C(..)` `id` on the heap: the class's type id and its fields'
    /// word types.
    fn news(&self, id: HirId) -> Option<&(u32, Vec<MirTy>)> {
        self.new_fact(id).object.as_ref()
    }

    /// A host native call lowering makes directly.
    fn host_call(&self, id: HirId) -> Option<&HirHost> {
        let cell = self.facts.hosts.get(id.0 as usize)?;
        cell.get_or_init(|| self.cx.host(id)).as_ref()
    }

    /// A heap field or `new` argument `id` whose slot holds a boxed enum
    /// object for a value whose own word is this niche layout.
    fn boxed_niche(&self, id: HirId) -> Option<ValueLayout> {
        let args = self.facts.new_arg_niches.get_or_init(|| {
            (0..self.hir.exprs.len() as u32).flat_map(|i| self.new_fact(HirId(i)).arg_niches.iter().copied()).collect()
        });
        args.get(&id.0).copied().or(match self.field(id).word {
            HirFieldWord::OpenNiche { layout, .. } => Some(layout),
            _ => None,
        })
    }
}

/// What lowering asks the compiler as it reaches each node: how the
/// checker lays out a type, and what a callee does. The compiler
/// implements it.
pub trait LowerCx {
    /// Whether a type is a class: its value is a heap object.
    fn is_class(&self, ty: &Ty) -> bool;
    /// The type id of a `readonly class` that moves as a pair (#1562): its
    /// box is an object of that class with the two words as its fields.
    fn pair_class(&self, ty: &Ty) -> Option<u32>;
    /// Whether a local of this type is pinned: it can hold an object with
    /// a `drop`, which must stay reachable as long as HIR emission's frame
    /// slot holds it (until the local is rebound or the function returns).
    fn pinned(&self, ty: &Ty) -> bool;
    /// The backing word of a scalar enum type (`#[repr(int)]` /
    /// `#[repr(float)]`).
    fn scalar_enum(&self, ty: &Ty) -> Option<MirTy>;
    /// A scalar enum variant's backing constant, by enum and variant name.
    fn scalar_variant(&self, enum_name: &str, variant: &str) -> Option<ScalarLit>;
    /// A user enum whose variants carry a drop hook (`TagEnumType` after
    /// `MakeEnum`), by name: its finalizer type id.
    fn enum_drop(&self, name: &str) -> Option<u32>;
    /// How a heap word of a type reads for the collector (`common::WORD_*`,
    /// as `layout::word_kind`): tuple and enum construction kinds.
    fn word_kind(&self, ty: &Ty) -> u8;
    /// The element kind an array of a type is stamped with at construction
    /// (`layout::array_elem_stamp`): packed bytes, pointer elements.
    fn array_stamp(&self, ty: &Ty) -> u8;
    /// For a boxed user enum type (one word, an `ObjEnum`, matched with
    /// `JumpIfMatch`): with no variant, `Some` of nothing; with a variant,
    /// its payload field types in declaration order. `None` for every other
    /// type.
    fn boxed_enum(&self, ty: &Ty, variant: Option<&str>) -> Option<Vec<Ty>>;
    /// Like [`Self::boxed_enum`] for any enum, `Option` or `Result` type:
    /// the view of a value of it boxed to one word (a pair kept in one
    /// local).
    fn boxed_view(&self, ty: &Ty, variant: Option<&str>) -> Option<Vec<Ty>>;
    /// The enum a two-word enum type pairs (not a tuple or range) and its
    /// variants' tags and payload word types (none for no payload), in
    /// declaration order.
    fn pair_variants(&self, ty: &Ty) -> Option<PairVariants>;
    /// The tags of a two-word enum type whose payload is `()` (bit `t`
    /// for tag `t`): the pair form carries `0` there, whatever word its
    /// box holds.
    fn unit_tags(&self, ty: &Ty) -> u64;
    /// A variant's payload field names in declaration order, by enum and
    /// variant name (a record pattern's fields in word order).
    fn payload_names(&self, enum_name: &str, variant: &str) -> Vec<String>;
    /// The word of a pointer-niche `Option` or `Result` type:
    /// [`MirTy::NicheOpt`] when the zero word is one side (`None`,
    /// `Ok(())`), [`MirTy::NicheRes`] when `Err` is the pointer with its
    /// low bit set.
    fn niche(&self, ty: &Ty) -> Option<MirTy>;
    /// Whether a function, by its HIR callee name, is pure: it writes no
    /// heap and does no I/O (it may trap or allocate).
    fn pure_fn(&self, name: &str) -> bool;
    /// Whether a function, by its HIR callee name, never resizes an array.
    fn steady_fn(&self, name: &str) -> bool;
    /// The word types of a two-word (pair) type.
    fn pair_tys(&self, ty: &Ty) -> Option<(MirTy, MirTy)>;
    /// Whether a two-word value of this type may carry a heap payload in
    /// its low word ([`MirFunc::heap_pair_words`]).
    fn pair_holds_heap(&self, ty: &Ty) -> bool;
    /// The element of a `Vec` of a closed two-word type: a lane array,
    /// each element's two words in place (`MirAllocKind::Lanes`,
    /// `LaneIndex` / `LaneStore` / `LanePush`).
    fn lane_elem(&self, ty: &Ty) -> Option<HirLaneElem>;
    /// A pair local's second-word slot and word types, by local.
    fn pair_local(&self, local: u32) -> Option<(MirTy, MirTy)>;
    /// The body's entry frame: where its parameters, captures and
    /// dictionaries sit, and where its own locals start.
    fn frame(&self) -> &BodyFrame;
    /// The frame's dictionaries are parameters of the body's MIR, each one
    /// heap word (a shared generic body's, a lambda's captured ones). A
    /// mono clone's frame keeps their slots, and nothing passes them.
    fn dict_params(&self) -> bool;
    /// The locals the body binds in its frame, besides its parameters. Any
    /// other binding is read as its value.
    fn bound_locals(&self) -> HashSet<u32>;
    /// The frame words of a local the body binds.
    fn local_words(&self, local: u32) -> LocalWords;
    /// What the compiler planned for node `id`. Asking has effects: it
    /// interns the node's strings, requests the method and instance clones
    /// its call goes to, reserves adapters and eq glue and builds
    /// dictionary statics. Lowering asks once per node, where it emits the
    /// node's code; a node it never reaches is never asked.
    fn site(&self, id: HirId) -> HirSite;
    /// The values the body packs as a bare-class existential.
    fn packs(&self) -> &HirPacks;
    /// Where the `kinds` word sits in a dictionary of trait `class`
    /// (after its methods, superclasses' included): bit 0 says whether the
    /// instance's type is a copy-on-write object (#2506, #2715).
    fn dict_kinds_index(&self, class: &str) -> Option<u32>;
    /// The `vec_from_array` native that copies fixed array local `value`
    /// as a `let` or assignment binds it to another local.
    fn array_copy(&self, value: HirId) -> Option<u16>;
    /// Whether `target` is the entry of a cold function (`#[cold]`, or
    /// never returning), as the site of a call to it found.
    fn cold_target(&self, target: Label) -> bool;
    /// No finalizer resizes an array ([`MirFunc::lengths_steady`]).
    fn alloc_steady(&self) -> bool;
    /// A function-local `const` table of literals (#1561): the static slot
    /// array `Make` `id` is built into on first use and loaded from after.
    fn frozen_array(&self, id: HirId) -> Option<u32>;
    /// The `unwind_resume` native a `defer`'s cleanup pad ends in.
    fn unwind_native(&self) -> Option<u16>;
    /// The `coro_close` native a `for` loop over a generator calls when
    /// it is left early.
    fn coro_close_native(&self) -> Option<u16>;
    /// Source file of the body, for statement locations.
    fn file(&self) -> Option<u32>;
    /// The program's id ([`DebugLoc::scope`]) of inline scope `scope`.
    fn inline_scope(&self, scope: common::InlineScope) -> u32 {
        let _ = scope;
        0
    }
    /// A debugger reads the body's locals from their frame words.
    fn debugger(&self) -> bool;
    /// A coverage compile: each source line keeps the code it emits.
    fn coverage(&self) -> bool;
    /// Called once with the frame lowering numbered (each local's first
    /// slot): refuses a body with a slot-kept local, field or two-word
    /// value lowering cannot type.
    fn check_frame(&self, slots: &[Option<u32>]) -> Result<(), Refusal>;
    /// The word types of the body's result when it returns two words.
    fn ret_pair(&self) -> Option<(MirTy, MirTy)>;
    /// How field read or write `id` reaches its slot, unless it is a field
    /// of a scalar-replaced local (`frame_slot`: kept in a frame slot).
    fn field(&self, id: HirId, frame_slot: bool) -> HirFieldFact;
    /// How the body's lane arrays are used: the `Vec` calls that work on
    /// their lanes and the arrays that leave for one-word code.
    fn lane_uses(&self) -> HirLaneUses;
    /// A generic body's type parameter values are opaque words it only
    /// moves (a raw scalar or a box, as the caller passed it).
    fn opaque_vars(&self) -> bool;
    /// A stack array local kept in frame slots: its length and element
    /// type.
    fn stack(&self, local: u32) -> Option<(usize, MirTy)>;
    /// A class local kept in frame slots: the type of each field's slot,
    /// from the local's own.
    fn sroa(&self, local: u32) -> Option<Vec<MirTy>>;
    /// The class of an `sroa` local, for its box where it escapes: the
    /// type id and field count.
    fn sroa_class(&self, local: u32) -> Option<(u32, u32)>;
    /// A field read or written on an `sroa` local: the local and the
    /// field's slot from the local's first.
    fn frame_field(&self, id: HirId) -> Option<(u32, u32)>;
    /// A field read or write on a record: the field's slot (records are
    /// slotted objects, their fields sorted by name) and word type.
    fn record_field(&self, id: HirId) -> Option<(u32, MirTy)>;
    /// A record literal whose every field holds the word its type says:
    /// the shape's `InitTyped` type id, and each field's slot and word
    /// type, in literal order.
    fn record(&self, id: HirId) -> Option<(u32, Vec<(u32, MirTy)>)>;
    /// A record `let` pattern that binds only locals and wildcards: each
    /// bound field's slot and word type (`None`: a wildcard), in pattern
    /// order.
    fn record_pat(&self, id: HirId) -> Option<Vec<Option<(u32, MirTy)>>>;
    /// What `new C(..)` `id` makes on the heap.
    fn new_object(&self, id: HirId) -> HirNew;
    /// A `Vec::push` call HIR emission inlines.
    fn push(&self, id: HirId) -> bool;
    /// A host native call whose arguments and result are plain (boxed
    /// layout) words.
    fn host(&self, id: HirId) -> Option<HirHost>;
    /// A `for x in xs` loop whose array the checker pinned: its reads need
    /// no bounds check.
    fn pinned_for(&self, id: HirId) -> bool;
    /// The niche layout of a result the body returns boxed (a test body's
    /// `Result`): each return builds the `ObjEnum`.
    fn ret_boxed_niche(&self) -> Option<ValueLayout>;
    /// The static slot of a `Global` read or assigned that is a static.
    fn static_slot(&self, id: HirId) -> Option<u32>;
    /// The tail slot of a two-word static: its second word, the first in
    /// the static's own slot.
    fn static_tail(&self, id: HirId) -> Option<u32>;
    /// The folded value of a `Global` that is a `const`.
    fn const_value(&self, id: HirId) -> Option<&crate::const_fold::ConstValue>;
    /// An element-wise array or tuple operator.
    fn aggregate(&self, id: HirId) -> Option<&crate::typechecking::AggregateArithInfo>;
    /// A call a `return` of the body may make as `TailCall`.
    fn tail_call(&self, id: HirId) -> bool;
    /// An `x.len()` call: a fixed size, or `None` for the array's length.
    fn len_of(&self, id: HirId) -> Option<Option<u32>>;
    /// The frame-slot locals that box before statement `id` (they escape
    /// there).
    fn box_at(&self, id: HirId) -> Option<&[u32]>;
    /// Whether early exits move out of line.
    fn cold_ok(&self) -> bool;
}

/// A host native call lowering makes directly ([`LowerCx::host`]): the
/// native id, its result layout, and for a two-word enum result each
/// variant's tag and payload arity in declaration order.
pub type HirHost = (u16, u8, Option<HostVariants>);

/// What a `new C(..)` makes on the heap ([`LowerCx::new_object`]).
#[derive(Clone, Debug, Default)]
pub struct HirNew {
    /// The class's type id and its fields' word types, when every field
    /// holds the word its type names.
    pub object: Option<(u32, Vec<MirTy>)>,
    /// Each field's tail slot when the class has flat two-word fields
    /// (`Checker::class_flat_tails`): its second word.
    pub tails: Vec<Option<u32>>,
    /// The arguments for a field open in a class parameter whose own word
    /// is a niche layout: boxed to the enum object the field holds.
    pub arg_niches: Vec<(u32, ValueLayout)>,
}

/// How a field read or write `e.f` reaches the field
/// ([`LowerCx::field`]).
#[derive(Clone, Copy, Debug, Default)]
pub struct HirFieldFact {
    pub word: HirFieldWord,
    /// A field of a `readonly class` instance: no store ever changes what
    /// it reads (#1560).
    pub invariant: bool,
}

/// The slot a field read or write uses ([`HirFieldFact`]).
#[derive(Clone, Copy, Debug, Default)]
pub enum HirFieldWord {
    /// None lowering reads directly.
    #[default]
    None,
    /// A flat two-word class field: its slot (the first word), its tail
    /// slot (the second) and their word types.
    Pair { slot: u32, tail: u32, lo: MirTy, hi: MirTy },
    /// A heap class object's field (or a record variant's payload) that
    /// holds the word its type says: its slot and word type.
    Heap { slot: u32, ty: MirTy },
    /// A generic class's field open in a parameter, read or written at a
    /// ground type with a niche layout of its own: its slot holds the
    /// boxed enum object, unboxed on the way out and boxed on the way in.
    OpenNiche { slot: u32, layout: ValueLayout },
}

/// How a body's lane arrays are used ([`LowerCx::lane_uses`]).
#[derive(Default)]
pub struct HirLaneUses {
    /// `Vec` method calls on a lane array, or making one, by call id.
    pub calls: HashMap<u32, HirLaneCall>,
    /// The lane arrays whose value goes on to code that reads it one word
    /// per element (a generic body, a host native, a container): a
    /// `LanesToWords` turns the array into that form first, in place.
    pub escapes: HashSet<u32>,
}

/// The backing word of a scalar enum variant, or an `int` match literal.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ScalarLit {
    /// An `int`, `float` or `bool` constant.
    Const(MirConst),
    /// A `#[repr(string)]` variant's string, by its string table index.
    Str(u32),
}

/// Each variant's tag and payload arity, in declaration order.
pub type HostVariants = Vec<(u32, u32)>;

/// A two-word enum's name and its variants' tags and payload word types
/// ([`LowerCx::pair_variants`]).
pub type PairVariants = (String, Vec<(u32, Option<MirTy>)>);

/// One stack operand of an FFI opcode ([`HirFfi`]), in push order.
#[derive(Clone, Debug)]
pub enum HirFfiOperand {
    /// A global static: an `extern`'s library or function id.
    Static(u32),
    /// An expression's word.
    Value(u32),
    /// A constant word: an FFI type tag.
    Const(i64),
    /// A function's code pointer: an `invoke` callback.
    CodePtr(Label),
    /// A tuple of these, built in order.
    Tuple(Vec<HirFfiOperand>),
}

/// An `extern` call or `dload` / `declare` / `invoke`, as HIR emission's
/// `HirBuiltin::Ffi` / `FfiDyn`: the opcode over its operands, then the
/// boxed `Result` it pushes unwrapped (an `extern`), converted to its
/// niche word, or kept as its object.
#[derive(Clone, Debug)]
pub struct HirFfi {
    pub op: super::inst::MirFfiOp,
    pub operand: u32,
    pub operands: Vec<HirFfiOperand>,
    /// An `extern`: `Ok`'s payload, or a panic with the error's message.
    pub unwrap: bool,
    /// The niche word the result converts to.
    pub niche: Option<MirTy>,
}

/// How `Lower::pair_eq` compares two two-word values' words.
enum PairEqWords {
    /// An enum pair: per variant, its tag and its payload's word type.
    Variants(Vec<(u32, Option<MirTy>)>),
    /// A product: its two words' types.
    Product(MirTy, MirTy),
}

/// What the compiler planned for one node ([`LowerCx::site`]): the entry
/// its code calls, the string it loads, the function value it makes.
/// Lowering asks once, as it reaches the node.
#[derive(Clone, Debug, Default)]
pub struct HirSite {
    /// String table index of a `string` literal, a `const` global folded
    /// to a string, a `typeof` name or a string index's panic message.
    pub string: Option<u32>,
    /// A generator's value nobody produced, at a type the word `0` is a
    /// value of (`Result<(), E>`, a number-backed scalar enum, #2717): the
    /// word standing for it. A `resume` or binding `yield` compares the word
    /// it reads with it; a gen fn's `return;` (and its body's end) returns
    /// it.
    pub missing: Option<i64>,
    /// The word a `resume` without `with`, or a `for` over a generator,
    /// sends where its receive type is such a type.
    pub missing_send: Option<i64>,
    /// A `format` call with only word arguments, or string `+` (as
    /// `"%s%s"`): the format string's index.
    pub format: Option<u32>,
    /// Per argument of such a `format` call: how a `%v` argument is first
    /// shown (its string then formats as `%s`).
    pub format_shows: Option<Vec<Option<HirShow>>>,
    /// A `matrix(data)` call: the data itself.
    pub matrix_wrap: bool,
    /// A matrix / vector operator or `dot` / `matmul` call on a packed
    /// kernel: its native and the meta word packing the shape.
    pub linalg: Option<(u16, u32)>,
    /// A `cross(a, b)` call unrolled: whether the result is a tuple, and
    /// whether its elements are floats or bytes.
    pub cross: Option<(bool, bool, bool)>,
    /// An `extern` call or `dload` / `declare` / `invoke`.
    pub ffi: Option<HirFfi>,
    /// A `dot` / `matmul` call or matrix operator unrolled (a shape past
    /// the packed kernel's limits).
    pub unrolled_la: Option<crate::typechecking::LinearAlgebraKind>,
    /// A plain call's entry and word ABI: a call, or a user operator on a
    /// raw instance method.
    pub call: Option<HirCallSite>,
    /// Of that call's arguments, the ones of a niche layout a shared
    /// generic takes in a boxed parameter: boxed to their enum object.
    pub niche_args: Vec<(u32, ValueLayout)>,
    /// A user operator that calls its trait instance method.
    pub op_call: Option<HirOpCall>,
    /// An element-wise operator at a user type (a clone of a generic
    /// body): the instance method each element calls, and whether it takes
    /// its operands raw (else as heap carriers' words).
    pub elem_call: Option<(HirCallSite, bool)>,
    /// A user operator with no instance: the raw opcode on the operands'
    /// words (`==` / `!=`, or a number-backed scalar enum's backing word).
    pub op_prim: bool,
    /// A call through a function value made a plain `CallIndirect` (not a
    /// `PolyFn` call with dictionaries).
    pub value_call: bool,
    /// Such a call of a two-word result read as its two words: its
    /// `unpair` body ([`MirInst::CallIndirect`]).
    pub value_pair_call: Option<Label>,
    /// A call through a `PolyFn` local.
    pub poly_call: Option<HirPolyCall>,
    /// A trait method or operator on a bound type parameter, called
    /// through the body's own dictionary.
    pub bound_call: Option<HirBoundCall>,
    /// A trait method on a bare-class existential pack: the method's slot
    /// in the pack's dictionary.
    pub existential_call: Option<u32>,
    /// A parallel loop emitted as a call of its fork-join function.
    pub par_loop: Option<HirParLoop>,
    /// An auto-par call site sent to a fork-join worker.
    pub par_fork: Option<HirParFork>,
    /// A lambda, a function read as a value or a partial application.
    pub fn_value: Option<HirFnValue>,
    /// The user `into_iter` entry a counted `for` loop calls.
    pub into_iter: Option<Label>,
    /// A `for` loop over a user iterator: its `into_iter` and `next`.
    pub iter_loop: Option<HirIterLoop>,
    /// A `new C(args).f` read whose object is never built: each argument
    /// runs in order and `f`'s word is the value.
    pub field_new: bool,
}

/// The values the body converts where it makes them ([`LowerCx::packs`]):
/// those it packs as a bare-class existential, and the function values it
/// stores where a shared body reads their `()` result as a type parameter.
#[derive(Debug, Default)]
pub struct HirPacks {
    /// By expression: the pack's program, run on the value's word
    /// (`[boxed value, dictionary]`).
    pub programs: HashMap<u32, Vec<HirDictOp>>,
    /// Some packed value has no program.
    pub unplanned: bool,
    /// By expression: a ground `-> ()` function value stored in a generic
    /// class's field or enum's payload declared `fn(..) -> V`, `V` a type
    /// parameter, wrapped in the adapter that returns the empty tuple
    /// object (`MakeFn` of its entry, the label, then the operand), as the
    /// type's shared methods read the result as their `V` (#2541).
    pub unit_fns: HashMap<u32, (Label, u32)>,
}

/// What lowering has asked [`LowerCx::site`] so far, by node. A body and
/// its cleanup pad share it.
struct Sites(Vec<OnceCell<HirSite>>);

/// The frame words of a local ([`LowerCx::local_words`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalWords {
    /// One slot per word: a plain local's one, a stack array's elements, a
    /// scalar-replaced class local's fields.
    Slots(u32),
    /// The two words of a pair.
    Pair,
}

/// The frame a body's MIR keeps its locals in: the entry words where
/// [`BodyFrame`] puts them, then each local the body binds, in local order,
/// one id per word. A local's [`LocalId`] is its frame slot.
#[derive(Debug, Default)]
pub struct LocalFrame {
    /// First id of each local that has words (`None`: never bound here).
    pub slots: Vec<Option<u32>>,
    /// Second id of each two-word (pair) local.
    pub tags: HashMap<u32, u32>,
    /// Words of each bound local.
    pub widths: HashMap<u32, u32>,
}

impl LocalFrame {
    fn new(hir: &HirBody, cx: &dyn LowerCx) -> Result<Self, Refusal> {
        let entry = cx.frame();
        let mut frame = LocalFrame { slots: vec![None; hir.locals.len()], ..LocalFrame::default() };
        // A lambda lowers with its captures as its first parameters.
        if entry.entry_locals().count() != hir.params.len() {
            return Err("param slot".into());
        }
        for (&param, local) in hir.params.iter().zip(entry.entry_locals()) {
            frame.slots[param.0 as usize] = Some(local.slot);
            frame.widths.insert(param.0, 1);
            if let Some((tag, _)) = local.pair {
                frame.tags.insert(param.0, tag);
            }
        }
        let bound = cx.bound_locals();
        let mut next = entry.frame_base;
        for local in 0..hir.locals.len() as u32 {
            if frame.slots[local as usize].is_some() || !bound.contains(&local) {
                continue;
            }
            let width = match cx.local_words(local) {
                LocalWords::Slots(n) => n,
                LocalWords::Pair => {
                    frame.tags.insert(local, next + 1);
                    2
                }
            };
            frame.slots[local as usize] = Some(next);
            frame.widths.insert(local, width);
            next += width;
        }
        cx.check_frame(&frame.slots)?;
        Ok(frame)
    }

    /// `local`'s first slot.
    fn slot(&self, local: HirLocal) -> Option<u32> {
        self.slots.get(local.0 as usize).copied().flatten()
    }
}

impl Sites {
    /// What the compiler planned for `id`, asked the first time.
    fn get(&self, cx: &dyn LowerCx, id: HirId) -> &HirSite {
        self.0[id.0 as usize].get_or_init(|| cx.site(id))
    }
}

impl<'a> Lower<'a> {
    /// What the compiler planned for node `id` ([`LowerCx::site`]).
    fn site(&self, id: HirId) -> &'a HirSite {
        self.sites.get(self.cx, id)
    }
}

struct Loop {
    /// Where `continue` goes: the loop head, or a `for` loop's step.
    cont: BlockId,
    /// Whether a `continue` jumped to `cont`.
    cont_used: bool,
    /// Where `break` goes, made at the first one.
    exit: Option<BlockId>,
    /// How many `defer`s were open at the loop: a `break` or `continue`
    /// runs the ones its body opened since.
    defers: usize,
}

/// One word of a boxed pair ([`Lower::pair_boxes`]): a value, or the
/// write of a local it was read from.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PairWord {
    Value(ValueId),
    Read(LocalId, u32),
}

/// An enum object boxed from a pair's words ([`Lower::pair_boxes`]).
struct PairBox {
    lo: PairWord,
    hi: PairWord,
    ty: Ty,
    /// The innermost loop the box was made in, by its `continue` target.
    in_loop: Option<BlockId>,
    /// The block the object is known in.
    block: BlockId,
    obj: ValueId,
}

/// What leaving a block early has to do, innermost last.
#[derive(Clone, Copy)]
enum Open {
    /// A `defer`: its armed flag and body.
    Defer(LocalId, HirId),
    /// A `for` loop over a generator: close the generator (a `break` out
    /// of it, or a `return` from its body).
    Coro(ValueId),
}

struct Lower<'a> {
    hir: &'a HirBody,
    /// Where the body's locals sit.
    frame: &'a LocalFrame,
    sites: &'a Sites,
    /// What lowering asks the compiler.
    cx: &'a dyn LowerCx,
    /// What lowering has asked so far, by node and local.
    facts: Facts,
    /// The word types of the body's result when it returns two words.
    ret_pair: Option<(MirTy, MirTy)>,
    /// The niche layout of a result the body returns boxed.
    ret_boxed_niche: Option<ValueLayout>,
    /// The body's type parameter values are opaque words it only moves.
    opaque_vars: bool,
    b: MirBuilder,
    /// The block being filled.
    cur: BlockId,
    loops: Vec<Loop>,
    /// The function returns `()`: a `return` gives the runtime's unit word.
    unit_ret: bool,
    /// The type a `return` gives: a `gen fn`'s yielded type (its final
    /// resume's word), else the declared one.
    ret_hir: Ty,
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
    /// Statement `match`es not yet lowered (one `effect` lowers, or the
    /// tail of a block whose value is dropped): nothing reads their value.
    stmt_matches: HashSet<HirId>,
    /// `resume`s and binding `yield`s whose value nothing reads (a block's
    /// tail, an `if` arm, lowered for its effect): no zero, no panic (#2717).
    unread: HashSet<HirId>,
    /// The arm bodies of statement `match`es: each one's value is dropped
    /// where the arm ends, as HIR emission did, so arms leaving different
    /// shapes (a word beside a block that falls through with nothing) need
    /// not meet in a φ.
    discard_arms: HashSet<HirId>,
    /// The source type of each arm value, by the block the arm ends in: a
    /// join of a boxed arm with pair arms unboxes it there.
    arm_tys: HashMap<BlockId, Ty>,
    /// Next local for a value HIR emission keeps in a temp slot (a `for`
    /// loop's index), numbered past any frame slot.
    next_temp: u32,
    /// Stack arrays boxed so far, in source order: the local holding each
    /// one's heap array.
    boxes: HashMap<u32, LocalId>,
    /// Enum objects made from a two-word pair so far. A later box of the
    /// same words where one dominates reuses its object (enum objects are
    /// immutable).
    pair_boxes: Vec<PairBox>,
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
    /// Niche words made from a known variant: whether it is the second
    /// (`Some` / `Err`) and its payload word.
    known_niche: HashMap<ValueId, (bool, Option<ValueId>)>,
    raw_host: bool,
    /// Each `defer`'s armed flag local, by its node; cleared at entry.
    defer_flags: HashMap<u32, LocalId>,
    /// The open `defer`s, in source order: flag and body. A `defer` runs
    /// at the end of its block, so a block's own are dropped from here as
    /// it ends; a `return` runs all of these, a `break` or `continue` those
    /// its loop body opened.
    defers: Vec<Open>,
    /// Every `defer` lowered, in source order, for the unwind pad: it runs
    /// the armed ones, and one whose block ended is disarmed.
    lowered_defers: Vec<(LocalId, HirId)>,
    /// `let`s in a `defer` body, which HIR emission slots in the thunk's
    /// own frame: a temp each.
    defer_locals: HashMap<u32, LocalId>,
    /// Pinned locals ([`LowerCx::pinned`]) by HIR local: their slot and
    /// word type. Each word stays live until the local is rebound and to
    /// every exit, as HIR emission's frame slot holds it.
    pins: HashMap<u32, (LocalId, MirTy)>,
    /// The homes a `defer`'s cleanup pad reads, when it gets one.
    pad: Option<PadHomes>,
    /// Existential packs built so far, by expression.
    packed: HashSet<u32>,
    /// The body's dictionary parameters, by frame slot.
    dict_params: HashMap<u32, ValueId>,
    /// A shared generic body's `kinds` word (#2506), or the one a lambda
    /// of it captures.
    kinds: Option<ValueId>,
    /// The `kinds` word's read out of the body's own dictionary, when that
    /// is all it is: dropped again where nothing uses it.
    kinds_load: Option<ValueId>,
    /// Words already lowered for these expressions: an element or field
    /// `x++` used for its value reads its place once, and its store takes
    /// the new word.
    preset: HashMap<u32, Val>,
    /// The `x++` lowered as a plain store (its value read separately).
    adjust_store: Option<HirId>,
    /// The program's id of each of the body's inline scopes, once asked.
    scopes: Vec<Option<u32>>,
}

impl<'a> Lower<'a> {
    /// A lowering of `hir` into `b`, at its entry block, for a body that
    /// returns `()` until the caller says otherwise.
    fn new(hir: &'a HirBody, cx: &'a dyn LowerCx, frame: &'a LocalFrame, sites: &'a Sites, b: MirBuilder) -> Self {
        let cur = b.entry();
        Lower {
            hir,
            frame,
            sites,
            cx,
            facts: Facts::new(hir),
            ret_pair: cx.ret_pair(),
            ret_boxed_niche: cx.ret_boxed_niche(),
            opaque_vars: cx.opaque_vars(),
            b,
            cur,
            loops: Vec::new(),
            unit_ret: false,
            ret_hir: Ty::Tuple(Vec::new()),
            order: vec![cur],
            cold: Vec::new(),
            aliases: HashMap::new(),
            ret_tail: false,
            stmt_matches: HashSet::new(),
            unread: HashSet::new(),
            discard_arms: HashSet::new(),
            arm_tys: HashMap::new(),
            next_temp: TEMP_BASE,
            boxes: HashMap::new(),
            pair_boxes: Vec::new(),
            unniche: HashMap::new(),
            spec: HashSet::new(),
            word_src: HashMap::new(),
            known_niche: HashMap::new(),
            raw_host: false,
            defer_flags: HashMap::new(),
            defers: Vec::new(),
            lowered_defers: Vec::new(),
            defer_locals: HashMap::new(),
            pins: HashMap::new(),
            pad: None,
            packed: HashSet::new(),
            dict_params: HashMap::new(),
            kinds: None,
            kinds_load: None,
            preset: HashMap::new(),
            adjust_store: None,
            scopes: vec![None; hir.inline_scopes.len()],
        }
    }
}

/// The answers lowering got from [`LowerCx`], each worked out the first
/// time it asks: by node, or by local.
struct Facts {
    pair_tys: Vec<OnceCell<Option<(MirTy, MirTy)>>>,
    pair_locals: Vec<OnceCell<Option<(u32, MirTy, MirTy)>>>,
    lanes: Vec<OnceCell<Option<HirLaneElem>>>,
    fields: Vec<OnceCell<HirFieldFact>>,
    lane_uses: OnceCell<HirLaneUses>,
    stacks: Vec<OnceCell<Option<(u32, usize, MirTy)>>>,
    sroa: Vec<OnceCell<Option<Vec<MirTy>>>>,
    frame_fields: Vec<OnceCell<Option<u32>>>,
    news: Vec<OnceCell<HirNew>>,
    new_arg_niches: OnceCell<HashMap<u32, ValueLayout>>,
    hosts: Vec<OnceCell<Option<HirHost>>>,
}

/// `n` empty answers.
fn cells<T>(n: usize) -> Vec<OnceCell<T>> {
    std::iter::repeat_with(OnceCell::new).take(n).collect()
}

impl Facts {
    fn new(hir: &HirBody) -> Self {
        Facts {
            pair_tys: cells(hir.exprs.len()),
            pair_locals: cells(hir.locals.len()),
            lanes: cells(hir.exprs.len()),
            fields: cells(hir.exprs.len()),
            lane_uses: OnceCell::new(),
            stacks: cells(hir.locals.len()),
            sroa: cells(hir.locals.len()),
            frame_fields: cells(hir.exprs.len()),
            news: cells(hir.exprs.len()),
            new_arg_niches: OnceCell::new(),
            hosts: cells(hir.exprs.len()),
        }
    }
}

/// Where a body's `defer` state sits for its unwind cleanup pad: each
/// flag's home, and each capture's (by HIR local) with its frame slot
/// (`None`: a slot-less binding, written where it is bound).
#[derive(Default)]
struct PadHomes {
    flags: HashMap<LocalId, u16>,
    caps: HashMap<u32, (u16, Option<LocalId>)>,
}

/// The MIR type of a scalar `ty`, or `None` when it is not one.
pub fn scalar(ty: &Ty) -> Option<MirTy> {
    match classify::primitive(ty)? {
        // A byte is its value as an int word (casts mask it).
        "int" | "byte" => Some(MirTy::I64),
        "float" => Some(MirTy::F64),
        "bool" => Some(MirTy::Bool),
        _ => None,
    }
}

/// The MIR type of a one-word value: a scalar, or a heap array or `Vec`
/// of scalars (`int` / `float` elements are raw words; `byte` / `bool`
/// ones are packed, and an index reads or writes the element's word).
pub fn word(ty: &Ty) -> Option<MirTy> {
    scalar(ty).or_else(|| array_elem(ty).and_then(scalar).map(|_| MirTy::HeapRef))
}

/// A pointer-niche `Option` / `Result` word type.
fn is_niche(ty: MirTy) -> bool {
    matches!(ty, MirTy::NicheOpt | MirTy::NicheRes)
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

/// Where an element-wise operand's elements come from.
enum AggSrc {
    /// Literal items or stack array slots, read when the operand runs.
    Elems(Vec<ValueId>),
    Pair(ValueId, ValueId),
    Word(ValueId),
}

/// One side of an element-wise operator: a broadcast's scalar, or elements.
enum AggSide {
    Scalar(ValueId),
    Elems(AggSrc),
}

/// What a counted array loop walks.
#[derive(Clone, Copy)]
enum Items {
    Array,
    /// A tuple's items, gathered into an array.
    Tuple(usize),
}

/// A non-empty tuple type, whose value is a heap tuple when it is not
/// kept as a pair.
fn is_heap_tuple(ty: &Ty) -> bool {
    matches!(strip_readonly(ty), Ty::Tuple(items) if !items.is_empty())
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

/// A GC `Root<T>` / `Weak<T>` handle: a heap object (`ObjRoot` / `ObjWeak`).
fn is_gc_handle(ty: &Ty) -> bool {
    matches!(strip_readonly(ty), Ty::App(head, args) if args.len() == 1
        && matches!(head.as_ref(), Ty::Con(n) if n == crate::typechecking::ty::ROOT || n == crate::typechecking::ty::WEAK))
}

/// An opaque `thread` module handle (`Thread`, `Sender`, `Receiver`,
/// `Mutex`, `RwLock`): a heap object.
fn is_thread(ty: &Ty) -> bool {
    crate::typechecking::ty::opaque_handle_name(ty).is_some_and(|n| n != crate::typechecking::ty::STREAM)
}

/// The element type of an array or `Vec`.
fn array_elem(ty: &Ty) -> Option<&Ty> {
    if let Some(data) = crate::typechecking::aggregate_arith::unwrap_matrix_ty(strip_readonly(ty)) {
        return array_elem(data);
    }
    match strip_readonly(ty) {
        Ty::Array { element, .. } => Some(element),
        _ => vec_element_ty(ty),
    }
}

/// `byte` itself (not a scalar enum backed by one).
fn is_byte(ty: &Ty) -> bool {
    matches!(strip_readonly(ty), Ty::Con(n) if n == crate::typechecking::ty::BYTE)
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

/// A value that is one opaque heap word whatever its use: a string,
/// record, IO stream, thread handle, coroutine or function value.
pub fn opaque_heap(ty: &Ty) -> bool {
    is_string(ty) || is_record(ty) || is_stream(ty) || is_thread(ty) || is_coroutine(ty) || is_fn(ty)
}

/// A function type: its values are `ObjFn` / `PolyFn` heap words.
fn is_fn(ty: &Ty) -> bool {
    match strip_readonly(ty) {
        Ty::Fun(..) => true,
        Ty::Forall { body, .. } => matches!(strip_readonly(body), Ty::Fun(..)),
        _ => false,
    }
}

/// A type variable applied to arguments: a higher-kinded `P<A>`.
fn is_var_app(ty: &Ty) -> bool {
    matches!(strip_readonly(ty), Ty::App(head, _) if is_var(head))
}

fn is_var(ty: &Ty) -> bool {
    matches!(strip_readonly(ty), Ty::Var(_))
}

/// A generic body moves its type parameter values but never reads them:
/// no operator, field, index or builtin takes one (any of them could read
/// a box as its payload) but a bound type parameter's dictionary method.
/// Calls pass them on as they are. The frame maps list the words it keeps
/// live past a collecting instruction ([`super::gc::live_opaque_words`]).
fn opaque_moves_only(hir: &HirBody, cx: &dyn LowerCx, sites: &Sites) -> Result<(), Refusal> {
    let opaque = |t: &Ty| is_var(t) || is_var_app(t);
    let var = |id: &HirId| hir.expr(*id).ty.as_ref().is_some_and(opaque);
    let any = hir.locals.iter().any(|l| l.ty.as_ref().is_some_and(opaque)) || hir.exprs.iter().any(|e| e.ty.as_ref().is_some_and(opaque));
    if !any {
        return Ok(());
    }
    // The code the body runs: a HIR pass may leave a replaced node behind.
    let mut reached = Vec::new();
    let mut todo: Vec<HirId> = hir.root.into_iter().collect();
    while let Some(id) = todo.pop() {
        reached.push(id);
        todo.extend(hir.children(id));
    }
    for id in reached {
        let e = hir.expr(id);
        let read = match &e.kind {
            HirKind::Bin { lhs, rhs, .. } | HirKind::Logic { lhs, rhs, .. } => var(lhs) || var(rhs),
            HirKind::Un { operand: x, .. } | HirKind::Cast { value: x } | HirKind::Field { base: x, .. } | HirKind::Spread(x) => var(x),
            HirKind::Index { base, index, .. } => var(base) || var(index),
            // A made object holds the word as HIR emission's does (the body
            // makes them without word kinds).
            HirKind::Make { .. } => false,
            // A call passes the word as it is to a parameter of the same
            // type variable (a generic callee or a closure's), as HIR
            // emission does; the callee reads nothing of it either.
            HirKind::Call { .. } => false,
            // A copy-on-write op on a type parameter's word passes it
            // through ([`FromHir::value`]): only a per-type clone knows
            // whether the word is an object (#2161).
            HirKind::Builtin { op: Builtin::Cow { .. }, .. } => false,
            HirKind::Builtin { args, .. } => args.iter().any(var),
            // A `yield` hands the word as it is to the resumer, which reads
            // it in the ground type's word (#2673).
            HirKind::Yield { .. } => false,
            HirKind::Named { value, .. } => var(value),
            HirKind::Append { base, value } => var(base) || var(value),
            // A store moves the word as it is, into a local, a field or a
            // type variable's array slot (`xs[i] = v` on a `Vec<T>`, stored
            // as the opaque word it holds).
            HirKind::Assign { place, value } => {
                var(value)
                    && !match &hir.expr(*place).kind {
                        HirKind::Local(_) | HirKind::Field { .. } => true,
                        HirKind::Index { base, index, .. } => !var(index) && hir.expr(*base).ty.as_ref().and_then(array_elem).is_some_and(is_var),
                        _ => false,
                    }
            }
            HirKind::If { cond, .. } => var(cond),
            HirKind::Match { scrutinee, .. } => var(scrutinee),
            HirKind::LetPat { init, .. } => var(init),
            _ => false,
        };
        // A bound operator's dictionary method takes the words as they are.
        if read && !(matches!(e.kind, HirKind::Bin { .. } | HirKind::Un { .. }) && sites.get(cx, id).bound_call.is_some()) {
            return Err("type variable read".into());
        }
    }
    Ok(())
}

/// `Result<(), E>`.
fn unit_result(ty: &Ty) -> bool {
    match strip_readonly(ty) {
        Ty::App(head, args) => matches!(head.as_ref(), Ty::Con(n) if n == "Result") && args.first().is_some_and(is_unit),
        Ty::Sum { name, variants } => {
            name == "Result"
                && variants.iter().any(|(v, payload)| v == "Ok" && matches!(payload, crate::typechecking::ty::EnumVariantPayloadTy::Tuple(items) if items.iter().all(is_unit)))
        }
        _ => false,
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

/// The word type of a niche layout.
fn niche_word(layout: ValueLayout) -> Result<MirTy, Refusal> {
    match layout {
        ValueLayout::NicheOption | ValueLayout::NicheUnitResult => Ok(MirTy::NicheOpt),
        ValueLayout::NicheResult => Ok(MirTy::NicheRes),
        ValueLayout::Boxed => Err("boxed niche layout".into()),
    }
}

fn mir(e: MirError) -> Refusal {
    format!("mir: {e}")
}

/// Lower `hir` to MIR. Parameters must sit in slots `0..n`, a pair
/// parameter in two.
pub fn lower_body(hir: &HirBody, cx: &dyn LowerCx) -> Result<(MirFunc, LocalFrame), Refusal> {
    let frame = LocalFrame::new(hir, cx)?;
    let sites = Sites(cells(hir.exprs.len()));
    let func = lower_in(hir, cx, &frame, &sites)?;
    Ok((func, frame))
}

fn lower_in(hir: &HirBody, cx: &dyn LowerCx, frame: &LocalFrame, sites: &Sites) -> Result<MirFunc, Refusal> {
    if hir.is_generic && !cx.opaque_vars() {
        return Err("body kind generic".into());
    }
    if !hir.captures.is_empty() {
        return Err("body kind closure".into());
    }
    let root = hir.root.ok_or("no root")?;
    let ret = hir.ret.as_ref().ok_or("return type")?;
    // A `gen fn` returns its final value as its coroutine's last resume.
    let ret = match strip_readonly(ret) {
        Ty::App(_, args) if hir.is_coro && is_coroutine(ret) => &args[0],
        _ => ret,
    };
    let mut b = MirBuilder::new(hir.name.clone());
    // Host natives are word edges.
    b.allow_host_edges = true;
    let mut lower = Lower::new(hir, cx, frame, sites, b);
    // A body that never returns (it only panics) leaves its return type a
    // type variable no word layout covers (a shared generic body's is a
    // word): nothing reads its word, so it returns as a unit body.
    let never_ret = matches!(strip_readonly(ret), Ty::Var(_))
        && lower.ret_pair.is_none()
        && lower.ret_boxed_niche.is_none()
        && lower.word_ty(ret).is_none()
        && cx.boxed_view(ret, None).is_none();
    let unit_ret = is_unit(ret) || never_ret;
    let ret_ty = match lower.ret_pair {
        _ if unit_ret => MirTy::I64,
        None if lower.ret_boxed_niche.is_some() => MirTy::HeapRef,
        Some((lo, _)) => lo,
        // A tuple not returned as a pair comes back boxed.
        None if matches!(strip_readonly(ret), Ty::Tuple(items) if !items.is_empty()) => MirTy::HeapRef,
        // So does any two-word value (a body called through a function
        // value, or depth-checked).
        None if lower.word_ty(ret).is_none() && cx.boxed_view(ret, None).is_some() => MirTy::HeapRef,
        None => lower.word_ty(ret).ok_or_else(|| format!("return type {}", ty_kind(ret)))?,
    };
    lower.unit_ret = unit_ret;
    lower.ret_hir = ret.clone();
    lower.b.set_ret_ty(ret_ty);
    // The entry words, in slot order: each parameter's (a lambda's
    // captures first), and each dictionary's when the body takes them.
    let entry = cx.frame();
    let takes_dicts = cx.dict_params();
    let dict_slots: HashSet<u32> = entry.dicts.values().copied().filter(|&slot| takes_dicts && slot < entry.entry_sp).collect();
    let mut next = 0;
    let mut dict_params = HashMap::new();
    // A shared generic entry's `kinds` word follows its dictionaries; a
    // lambda's follows the ones it captures.
    let kinds_slot = entry.kinds.as_ref().and_then(|k| k.slot);
    let mut kinds = None;
    let mut add_dicts = |lower: &mut Lower, next: &mut u32| -> Result<(), Refusal> {
        loop {
            if dict_slots.contains(next) {
                dict_params.insert(*next, lower.b.add_param(MirTy::HeapRef).map_err(mir)?);
            } else if kinds_slot == Some(*next) && kinds.is_none() {
                kinds = Some(lower.b.add_param(MirTy::I64).map_err(mir)?);
            } else {
                return Ok(());
            }
            *next += 1;
        }
    };
    for &param in &hir.params {
        add_dicts(&mut lower, &mut next)?;
        if frame.slot(param) != Some(next) {
            return Err("param slot".into());
        }
        let words = match lower.pair_local_tys(param) {
            Some((hi, lo_ty, hi_ty)) if hi == next + 1 => vec![lo_ty, hi_ty],
            Some(_) => return Err("param slot".into()),
            None => {
                let ty = hir.local(param).ty.as_ref().ok_or("param type")?;
                // A two-word type or tuple not passed as a pair comes boxed;
                // a `()` comes as the empty tuple object.
                let word = lower.word_ty(ty)
                    .or_else(|| is_unit(ty).then_some(MirTy::HeapRef))
                    .or_else(|| cx.boxed_view(ty, None).map(|_| MirTy::HeapRef))
                    .or_else(|| matches!(strip_readonly(ty), Ty::Tuple(items) if !items.is_empty()).then_some(MirTy::HeapRef));
                vec![word.ok_or_else(|| format!("param type {}", ty_kind(ty)))?]
            }
        };
        let heap_pair = hir.local(param).ty.as_ref().is_some_and(|t| cx.pair_holds_heap(t));
        for (i, ty) in words.into_iter().enumerate() {
            let v = lower.b.add_param(ty).map_err(mir)?;
            lower.b.def_local(LocalId(next), v).map_err(mir)?;
            if heap_pair && i == 0 {
                lower.b.mark_heap_pair_word(v);
            }
            next += 1;
        }
    }
    // Dictionaries: read only where a dictionary program loads one.
    add_dicts(&mut lower, &mut next)?;
    if kinds_slot.is_some() && kinds.is_none() {
        return Err("kinds slot".into());
    }
    let kinds_frame = entry.kinds.as_ref();
    let dict_n = kinds_frame.and_then(|f| f.dict).map_or(0, |d| d.2);
    if let (Some(k), Some(f)) = (kinds, kinds_frame)
        && let Some(msg) = f.check
    {
        lower.check_kinds(k, f.n - dict_n, msg)?;
    }
    if takes_dicts {
        if next != entry.entry_sp {
            return Err("param slot".into());
        }
        // An instance method's context dictionaries: each read out of its
        // own dictionary at entry, into its slot.
        for &(slot, own, index) in &entry.context {
            let own = *dict_params.get(&own).ok_or("context dictionary")?;
            let dict = lower.b.ins_tuple_item(own, index, MirTy::HeapRef).map_err(mir)?;
            dict_params.insert(slot, dict);
        }
    }
    // A trait method's `kinds` word out of its own dictionary, its bits
    // first and the entry word's after them (#2506).
    if let Some(f) = kinds_frame
        && let Some((own, index, n)) = f.dict
    {
        let own = *dict_params.get(&own).ok_or("kinds dictionary")?;
        let d = lower.b.ins_tuple_item(own, index, MirTy::I64).map_err(mir)?;
        if let Some(msg) = f.check {
            lower.check_kinds(d, crate::codegen::KINDS_DICT_BITS, msg)?;
        }
        if kinds.is_none() && f.check.is_none() {
            lower.kinds_load = Some(d);
        }
        kinds = Some(match kinds {
            None => d,
            Some(p) => lower.merge_kinds(d, n, p, f.n)?,
        });
    }
    lower.kinds = kinds;
    lower.dict_params = dict_params;
    lower.pin_locals()?;
    for (i, e) in hir.exprs.iter().enumerate() {
        if let HirKind::Defer { captures, body } = &e.kind {
            let _ = captures;
            // The body's own bindings (`let`, a `for` variable, a pattern)
            // live in the thunk's frame, not the function's (#2134).
            let mut binds = Vec::new();
            hir.visit(*body, &mut |x| match &x.kind {
                HirKind::Let { local, .. } => binds.push(*local),
                HirKind::LetPat { pat, .. } | HirKind::ForIn { pat, .. } => {
                    pat.for_each_bind(&mut |l| binds.push(l));
                }
                HirKind::Match { arms, .. } => {
                    arms.iter().for_each(|a| a.pat.for_each_bind(&mut |l| binds.push(l)));
                }
                _ => {}
            });
            let lets: Vec<u32> = binds
                .into_iter()
                .filter(|&l| frame.slot(l).is_none())
                .map(|l| l.0)
                .collect();
            for l in lets {
                let t = lower.temp();
                lower.defer_locals.insert(l, t);
            }
            let flag = lower.temp();
            let off = lower.b.ins_const(MirConst::Bool(false)).map_err(mir)?;
            lower.b.def_local(flag, off).map_err(mir)?;
            lower.defer_flags.insert(i as u32, flag);
        }
    }
    if !lower.defer_flags.is_empty() && cx.unwind_native().is_some() {
        lower.pad = lower.pad_homes();
    }
    let params = lower.b.func().params.len();
    if hir.is_coro {
        // A `gen fn` runs its body for effect (a tail `yield` receives
        // nothing) and falling off its end returns `0` as its last
        // resume's word, as HIR emission's epilogue.
        if lower.effect(root)? {
            lower.ret_missing(root)?;
        }
    } else {
        match lower.value(root)? {
            Val::Never => {}
            // A valued body the checker lets end without a return (a
            // `while` on a constant `true`) never gets here.
            Val::Unit if !lower.unit_ret && lower.ret_boxed_niche.is_none() && !unit_result(&lower.ret_hir) => {
                lower.b.unreachable().map_err(mir)?;
            }
            val => lower.ret(val)?,
        }
    }
    let order = lower.layout();
    let spec = std::mem::take(&mut lower.spec);
    let pad = lower.pad.take();
    let defers = std::mem::take(&mut lower.lowered_defers);
    let defer_locals = std::mem::take(&mut lower.defer_locals);
    let next_temp = lower.next_temp;
    let boxed: HashSet<u32> = lower.boxes.keys().copied().collect();
    let packs = cx.packs();
    let unpacked = packs.programs.keys().any(|k| !lower.packed.contains(k));
    let kinds_load = lower.kinds_load;
    let mut func = lower.b.finish().map_err(mir)?;
    func.lengths_steady = cx.alloc_steady();
    // A dictionary `kinds` word the body never reads costs no load.
    if let Some(load) = kinds_load
        && !func.blocks.iter().any(|b| b.insts.iter().any(|i| i.operands().contains(&load)) || b.term.as_ref().is_some_and(|t| t.uses().any(|v| v == load)))
    {
        for b in &mut func.blocks {
            b.insts.retain(|i| !matches!(i, MirInst::ProductLoad { dest, .. } if *dest == load));
        }
    }
    // A capture read at a `return` some path reaches without passing its
    // `let` would be a live-in word.
    if func.params.len() != params {
        return Err("defer capture unbound on a path".into());
    }
    prune(&mut func, &spec);
    tidy(&mut func, &order);
    // A pad is an extra: a body without one stays pinned to HIR emission.
    if let (Some(pad), Some(native)) = (pad, cx.unwind_native())
        && !pad.caps.keys().any(|l| boxed.contains(l))
        && let Ok(cleanup) = lower_pad(hir, cx, frame, sites, PadParts { defers: &defers, defer_locals, next_temp, homes: &pad, home_tys: &func.homes, native })
    {
        func.cleanup = Some(Box::new(cleanup));
    }
    func.debug_keep = cx.debugger();
    func.keep_lines = cx.coverage();
    if cx.opaque_vars() {
        opaque_moves_only(hir, cx, sites)?;
        func.opaque_words = true;
    }
    // HIR emission packs a concrete value where a bare-class existential
    // is expected (`[boxed value, dictionary]`): with no program for a
    // pack, or a pack this lowering passed by, no tier emits from it.
    if packs.unplanned || unpacked {
        func.emit_refusal = Some("existential pack");
    }
    Ok(func)
}

/// What [`lower_pad`] builds a body's cleanup pad from.
struct PadParts<'p> {
    /// The body's `defer`s, in the order they were lowered.
    defers: &'p [(LocalId, HirId)],
    defer_locals: HashMap<u32, LocalId>,
    /// The body's next temporary: `defer_locals` are temporaries of the
    /// body, so the pad's own (a `for` loop's counter) start past them.
    next_temp: u32,
    homes: &'p PadHomes,
    /// The body's homes' word types.
    home_tys: &'p [MirTy],
    /// `unwind_resume`.
    native: u16,
}

/// The unwind cleanup pad of a body with `defer`s: each armed `defer`, last first, with its captures
/// read from their homes as they stood at the panic, then `unwind_resume`.
/// It runs in the body's frame and reads the homes there.
fn lower_pad(hir: &HirBody, cx: &dyn LowerCx, frame: &LocalFrame, sites: &Sites, parts: PadParts<'_>) -> Result<MirFunc, Refusal> {
    let mut b = MirBuilder::new(format!("{}$pad", hir.name));
    b.allow_host_edges = true;
    b.set_ret_ty(MirTy::I64);
    for &ty in parts.home_tys {
        b.add_home(ty).map_err(mir)?;
    }
    let mut pad = Lower::new(hir, cx, frame, sites, b);
    pad.unit_ret = true;
    pad.next_temp = parts.next_temp;
    pad.defer_locals = parts.defer_locals;
    let captures_of = |body: HirId| {
        hir.exprs.iter().find_map(|e| match &e.kind {
            HirKind::Defer { captures, body: b } if *b == body => Some(captures.clone()),
            _ => None,
        })
    };
    for &(flag, body) in parts.defers.iter().rev() {
        let home = *parts.homes.flags.get(&flag).ok_or("pad flag")?;
        let armed = pad.b.ins_home_load(home).map_err(mir)?;
        let (run, skip) = (pad.b.create_block(), pad.b.create_block());
        pad.b.branch(armed, run, skip).map_err(mir)?;
        pad.switch(run);
        let captures: Vec<HirLocal> = captures_of(body).ok_or("pad defer")?.into_iter().flatten().collect();
        for &c in &captures {
            let &(home, slot) = parts.homes.caps.get(&c.0).ok_or("pad capture")?;
            let v = pad.b.ins_home_load(home).map_err(mir)?;
            match slot {
                Some(slot) => pad.b.def_local(slot, v).map_err(mir)?,
                None => {
                    pad.aliases.insert(c.0, v);
                }
            }
        }
        // A `use (mut x)` the body assigns writes `x`'s home back, so the
        // pads of outer blocks read the new value.
        let mut assigned = Vec::new();
        hir.visit(body, &mut |x| {
            if let HirKind::Assign { place, .. } = x.kind
                && let HirKind::Local(l) = hir.expr(place).kind
                && captures.contains(&l)
                && !assigned.contains(&l)
            {
                assigned.push(l);
            }
        });
        if pad.stmt(body)? {
            for l in &assigned {
                let &(home, slot) = parts.homes.caps.get(&l.0).ok_or("pad capture")?;
                let slot = slot.ok_or("pad assigned capture without a slot")?;
                let ty = *parts.home_tys.get(usize::from(home)).ok_or("pad home")?;
                let v = pad.b.use_local(slot, ty).map_err(mir)?;
                pad.b.ins_home_store(home, v).map_err(mir)?;
            }
            pad.jump(skip)?;
        }
        pad.switch(skip);
    }
    pad.b.ins_host_invoke(parts.native, Vec::new()).map_err(mir)?;
    let zero = pad.b.ins_const(MirConst::I64(0)).map_err(mir)?;
    pad.b.ret(Some(zero)).map_err(mir)?;
    let order = pad.layout();
    let spec = std::mem::take(&mut pad.spec);
    let mut func = pad.b.finish().map_err(mir)?;
    if !func.params.is_empty() {
        return Err("pad live-in".into());
    }
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
pub(super) fn tidy(func: &mut MirFunc, order: &[BlockId]) {
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
    func.relayout(&layout);
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
    /// Give each `defer` flag and capture a home, written from here on at
    /// each definition (a parameter's now). `None` when a capture is not a
    /// one-word local: the body then has no pad.
    fn pad_homes(&mut self) -> Option<PadHomes> {
        let mut caps: Vec<HirLocal> = Vec::new();
        for e in &self.hir.exprs {
            if let HirKind::Defer { captures, .. } = &e.kind {
                for c in captures {
                    let c = (*c)?;
                    if !caps.contains(&c) {
                        caps.push(c);
                    }
                }
            }
        }
        let mut pad = PadHomes::default();
        let mut flags: Vec<(u32, LocalId)> = self.defer_flags.iter().map(|(&k, &v)| (k, v)).collect();
        flags.sort_unstable();
        let mut stores: Vec<(u16, LocalId, MirTy)> = Vec::new();
        for (_, flag) in flags {
            let home = self.b.add_home(MirTy::Bool).ok()?;
            pad.flags.insert(flag, home);
            stores.push((home, flag, MirTy::Bool));
        }
        for c in caps {
            if self.pair_local(c).is_some() || self.unit_local(c) || self.defer_locals.contains_key(&c.0) {
                return None;
            }
            let ty = self.local_ty(c).ok()?;
            let slot = self.frame.slot(c).map(LocalId);
            let home = self.b.add_home(ty).ok()?;
            pad.caps.insert(c.0, (home, slot));
            if let Some(slot) = slot
                && self.hir.params.contains(&c)
            {
                stores.push((home, slot, ty));
            }
        }
        for (home, local, ty) in stores {
            let v = self.b.use_local(local, ty).ok()?;
            self.b.ins_home_store(home, v).ok()?;
        }
        for (&flag, &home) in &pad.flags {
            self.b.homed.insert(flag, home);
        }
        for &(home, slot) in pad.caps.values() {
            if let Some(slot) = slot {
                self.b.homed.insert(slot, home);
            }
        }
        Some(pad)
    }

    fn slot(&self, local: HirLocal) -> Result<LocalId, Refusal> {
        self.frame
            .slot(local)
            .map(LocalId)
            .or_else(|| self.defer_locals.get(&local.0).copied())
            .ok_or_else(|| "local slot".into())
    }

    /// The type of a one-word local. A stack array or class local is
    /// slots, not a word.
    fn local_ty(&self, local: HirLocal) -> Result<MirTy, Refusal> {
        if self.stack(local.0).is_some() || self.sroa(local.0).is_some() {
            return Err("frame-slot local value".into());
        }
        let ty = self.hir.local(local).ty.as_ref().ok_or("local type")?;
        self.word_ty(ty)
            .or_else(|| self.boxed_local(local).then_some(MirTy::HeapRef))
            // A tuple not kept as a pair is a heap tuple.
            .or_else(|| {
                (self.pair_local_tys(local).is_none() && matches!(strip_readonly(ty), Ty::Tuple(items) if !items.is_empty()))
                    .then_some(MirTy::HeapRef)
            })
            .ok_or_else(|| format!("local type {}", ty_kind(ty)))
    }

    /// A local of type `()`: nothing to bind or read.
    fn unit_local(&self, local: HirLocal) -> bool {
        self.hir.local(local).ty.as_ref().is_some_and(is_unit)
    }

    /// `local` is a `for mut` binding of an array's object items at a
    /// ground type, which the loop makes unique in place (#2782). An open
    /// type's or an existential's item keeps the one word it has.
    fn for_mut_item(&self, local: HirLocal, elem: MirTy, array: bool) -> bool {
        let l = self.hir.local(local);
        array
            && l.borrow
            && elem == MirTy::HeapRef
            && l.ty.as_ref().map(strip_readonly).is_some_and(|t| !is_var(t) && !is_var_app(t) && !matches!(t, Ty::Existential { .. }))
    }

    /// A local of a two-word type that HIR emission keeps boxed in one slot.
    fn boxed_local(&self, local: HirLocal) -> bool {
        self.pair_local_tys(local).is_none()
            && self.hir.local(local).ty.as_ref().is_some_and(|t| self.cx.boxed_view(t, None).is_some())
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

    /// `RETURN` of `v`, after the `defer`s lowered so far, last first.
    fn ret_word(&mut self, v: ValueId) -> Result<(), Refusal> {
        self.run_defers()?;
        self.keep_pins()?;
        self.b.ret(Some(v)).map_err(mir)
    }

    /// Two-word `RETURN`, after the `defer`s as [`Self::ret_word`].
    fn ret_two(&mut self, lo: ValueId, hi: ValueId) -> Result<(), Refusal> {
        self.run_defers()?;
        self.keep_pins()?;
        self.b.ret_pair(lo, hi).map_err(mir)
    }

    /// Find the pinned locals. One bound inside a nested block (a branch
    /// or loop) starts as a zero word, so the word it is bound to is still
    /// read at an exit or rebinding past the block's join.
    fn pin_locals(&mut self) -> Result<(), Refusal> {
        // Locals the body's own statements bind: they dominate what follows.
        let top: HashSet<u32> = match self.hir.root.map(|r| &self.hir.expr(r).kind) {
            Some(HirKind::Block { stmts, .. }) => stmts
                .iter()
                .filter_map(|&st| match self.hir.expr(st).kind {
                    HirKind::Let { local, .. } => Some(local.0),
                    _ => None,
                })
                .collect(),
            _ => HashSet::new(),
        };
        for (i, local) in self.hir.locals.iter().enumerate() {
            let id = HirLocal(i as u32);
            if !local.ty.as_ref().is_some_and(|t| self.cx.pinned(t))
                || self.frame.slot(id).is_none()
                || self.pair_local_tys(id).is_some()
            {
                continue;
            }
            let Ok(ty) = self.local_ty(id) else {
                continue;
            };
            if !matches!(ty, MirTy::HeapRef | MirTy::NicheOpt | MirTy::NicheRes) {
                continue;
            }
            let slot = self.slot(id)?;
            if !self.hir.params.contains(&id) && !top.contains(&id.0) {
                let zero = self.b.ins_const(MirConst::I64(0)).map_err(mir)?;
                let zero = self.fit(zero, ty)?;
                self.b.def_local(slot, zero).map_err(mir)?;
            }
            self.pins.insert(id.0, (slot, ty));
        }
        Ok(())
    }

    /// Keep the word pinned local `local` holds now live to here (none
    /// before its first binding).
    fn keep_pin(&mut self, local: HirLocal) -> Result<(), Refusal> {
        let Some(&(slot, ty)) = self.pins.get(&local.0) else {
            return Ok(());
        };
        if !self.b.local_defined(slot) {
            return Ok(());
        }
        let v = self.b.use_local(slot, ty).map_err(mir)?;
        self.b.ins_keep_alive(v).map_err(mir)?;
        Ok(())
    }

    /// The heap word `slot` holds before a rebind, for a `ClearWord` once the
    /// new value is bound (#1402). HIR emission overwrites the local's frame
    /// slot, which ends the old value's lifetime; here the new value gets a
    /// register of its own, and the old one would stay in its register (a
    /// root to the end of the frame) unless zeroed. Emission skips the zero
    /// while another live value shares that register.
    fn rebound_word(&mut self, slot: LocalId, ty: MirTy) -> Result<Option<ValueId>, Refusal> {
        if !ty.is_heap_word() || !self.b.local_defined(slot) {
            return Ok(None);
        }
        Ok(Some(self.b.use_local(slot, ty).map_err(mir)?))
    }

    /// Keep every pinned local's word live to here (an exit).
    fn keep_pins(&mut self) -> Result<(), Refusal> {
        let mut pins: Vec<u32> = self.pins.keys().copied().collect();
        pins.sort_unstable();
        for local in pins {
            self.keep_pin(HirLocal(local))?;
        }
        Ok(())
    }

    /// Each open armed `defer`, last first: disarm it and run its body
    /// inline (a `return`).
    fn run_defers(&mut self) -> Result<(), Refusal> {
        self.run_defers_from(0)
    }

    /// The open armed `defer`s from the `start`th on, last first: disarm
    /// each and run its body inline. The armed flag guards a `defer` that
    /// a path to here did not reach.
    fn run_defers_from(&mut self, start: usize) -> Result<(), Refusal> {
        for k in (start..self.defers.len()).rev() {
            let (flag, body) = match self.defers[k] {
                Open::Defer(flag, body) => (flag, body),
                Open::Coro(h) => {
                    let native = self.cx.coro_close_native().ok_or("coro close native")?;
                    self.b.ins_host_invoke(native, vec![h]).map_err(mir)?;
                    continue;
                }
            };
            let armed = self.b.use_local(flag, MirTy::Bool).map_err(mir)?;
            let (run, skip) = (self.b.create_block(), self.b.create_block());
            self.b.branch(armed, run, skip).map_err(mir)?;
            self.switch(run);
            let off = self.b.ins_const(MirConst::Bool(false)).map_err(mir)?;
            self.b.def_local(flag, off).map_err(mir)?;
            if self.stmt(body)? {
                self.jump(skip)?;
            }
            self.switch(skip);
        }
        Ok(())
    }

    fn ret_unit(&mut self) -> Result<(), Refusal> {
        let zero = self.b.ins_const(MirConst::I64(0)).map_err(mir)?;
        self.ret_word(zero)
    }

    /// `v` as the function returns it: a two-word value the function
    /// returns boxed is built boxed.
    fn ret_value(&mut self, v: HirId) -> Result<Val, Refusal> {
        if self.ret_pair.is_none() && self.b.func().ret_ty == Some(MirTy::HeapRef) && self.pair_tys(v).is_some() {
            // The box is what returns, not the first value lowered inside
            // it: clear the returned flag first, or a payload `match`
            // returns its arms' words (#1164: `return Some(e?)` in a
            // lambda). A `None` made boxed lowers no value at all; left
            // set, the flag would leak into the next statement (#1271).
            self.ret_tail = false;
            let w = self.boxed_word(v)?;
            return Ok(match w {
                Some(w) => Val::V(w),
                None => Val::Never,
            });
        }
        self.value(v)
    }

    /// Return `val`, which must fit the function's result.
    fn ret(&mut self, val: Val) -> Result<(), Refusal> {
        if let Some(layout) = self.ret_boxed_niche {
            let boxed = match val {
                // `Ok(())` boxed: `CONST 0; MakeEnum 0 1`, as HIR emission.
                Val::Unit if unit_result(&self.ret_hir) => {
                    let zero = self.b.ins_const(MirConst::I64(0)).map_err(mir)?;
                    self.make_enum(0, Some(zero))?
                }
                // Boxed from the word's own layout, as HIR emission
                // converts from the value's: a niche `Result` word (its Err
                // bit) may return as a `Result<(), E>`.
                Val::V(v) => {
                    let layout = match self.b.value_ty(v) {
                        MirTy::NicheRes => ValueLayout::NicheResult,
                        _ => layout,
                    };
                    self.box_niche(layout, v)?
                }
                _ => return Err("return value".into()),
            };
            return self.ret_word(boxed);
        }
        match (val, self.ret_pair) {
            (Val::Unit, _) if self.unit_ret => self.ret_unit(),
            // Falling off a `Result<(), E>` body is `Ok(())`: `0`, as HIR
            // emission's `CONST 0; RETURN`.
            (Val::Unit, None) if unit_result(&self.ret_hir) => {
                // `Ok(())` is the zero word of a `NicheOpt` result.
                if self.b.func().ret_ty != Some(MirTy::NicheOpt) {
                    return Err("return value".into());
                }
                let ok = self.b.ins_const(MirConst::I64(0)).map_err(mir)?;
                let ok = self.b.ins_cast(MirCastKind::Word, MirTy::NicheOpt, ok).map_err(mir)?;
                self.ret_word(ok)
            }
            // A unit body's tail value (a call kept for its effect) is dropped.
            (Val::V(_) | Val::Pair(..), _) if self.unit_ret => self.ret_unit(),
            // A niche word of the other niche layout has other bits.
            (Val::V(v), None) if self.b.func().ret_ty.is_some_and(|t| is_niche(t) && is_niche(self.b.value_ty(v)) && t != self.b.value_ty(v)) => {
                Err("return niche word".into())
            }
            (Val::V(v), None) if !self.unit_ret => self.ret_word(v),
            // A two-word value the function returns boxed.
            (Val::Pair(lo, hi), None) if self.b.func().ret_ty == Some(MirTy::HeapRef) => {
                let ty = self.ret_hir.clone();
                match self.box_pair_val(&ty, lo, hi)? {
                    Val::V(v) => self.ret_word(v),
                    _ => Err("return value".into()),
                }
            }
            // A boxed value the function returns as a pair.
            (Val::V(v), Some((lo_ty, hi_ty))) if self.b.value_ty(v) == MirTy::HeapRef => {
                let ty = self.ret_hir.clone();
                match self.unbox_word(&ty, v, lo_ty, hi_ty)? {
                    Val::Pair(lo, hi) => self.ret_two(lo, hi),
                    _ => Err("return value".into()),
                }
            }
            (Val::Pair(lo, hi), Some((lo_ty, hi_ty))) => {
                if self.b.value_ty(lo) != lo_ty || self.b.value_ty(hi) != hi_ty {
                    return Err("return pair type".into());
                }
                self.ret_two(lo, hi)
            }
            // A unit body whose tail is a value, or a valued body ending in `()`.
            _ => Err("return value".into()),
        }
    }

    /// The niche layout of `id`'s word of type `ty`: a `NicheRes` word is
    /// a niche `Result`, a `NicheOpt` one an `Option` or a `Result<(), E>`.
    fn niche_layout(&self, id: HirId, ty: MirTy) -> Result<ValueLayout, Refusal> {
        let result = self.expr_ty(id).is_some_and(|t| crate::typechecking::ty::is_result_ty(strip_readonly(t)));
        match ty {
            MirTy::NicheRes => Ok(ValueLayout::NicheResult),
            MirTy::NicheOpt if result => Ok(ValueLayout::NicheUnitResult),
            MirTy::NicheOpt => Ok(ValueLayout::NicheOption),
            _ => Err("niche layout".into()),
        }
    }

    /// The MIR type of a one-word value of `ty`: its [`Self::word_ty`],
    /// or the heap object an enum or tuple with no such word moves as.
    fn word_or_object(&self, ty: &Ty) -> Option<MirTy> {
        self.word_ty(ty).or_else(|| (self.cx.boxed_view(ty, None).is_some() || is_heap_tuple(ty)).then_some(MirTy::HeapRef))
    }

    /// A niche `Option` / `Result` word boxed to its `ObjEnum`.
    #[track_caller]
    fn box_niche(&mut self, layout: ValueLayout, w: ValueId) -> Result<ValueId, Refusal> {
        super::box_report::note(&self.b.func().name, super::box_report::BoxKind::Niche, || format!("{layout:?}"));
        if !matches!(self.b.value_ty(w), MirTy::NicheOpt | MirTy::NicheRes) {
            return Err("boxed niche return".into());
        }
        // A word made from a known variant boxes as that variant.
        if let Some(&(second, payload)) = self.known_niche.get(&w) {
            let payload = match payload {
                Some(p) => Some(self.fit(p, MirTy::HeapRef)?),
                None if layout == ValueLayout::NicheUnitResult && !second => Some(self.b.ins_const(MirConst::I64(0)).map_err(mir)?),
                None => None,
            };
            return self.make_enum(u32::from(second), payload);
        }
        let zero = self.b.ins_const(MirConst::I64(0)).map_err(mir)?;
        // Whether `w` is the second variant: `Some` (non-zero), or `Err`
        // (non-zero, or the low bit of a niche `Result`).
        let second = match layout {
            ValueLayout::NicheResult => {
                let one = self.b.ins_const(MirConst::I64(1)).map_err(mir)?;
                let bit = self.b.ins_binop(MirBinOp::BitAnd, w, one).map_err(mir)?;
                self.b.ins_cmp(MirCmpOp::Ne, bit, zero).map_err(mir)?
            }
            ValueLayout::NicheOption | ValueLayout::NicheUnitResult => self.b.ins_cmp(MirCmpOp::Ne, w, zero).map_err(mir)?,
            ValueLayout::Boxed => return Err("boxed niche layout".into()),
        };
        let (hit, fall) = (self.b.create_block(), self.b.create_block());
        self.b.branch(second, hit, fall).map_err(mir)?;
        let mut out = Vec::with_capacity(2);
        self.switch(fall);
        let first = match layout {
            ValueLayout::NicheOption => self.make_enum(0, None)?,
            ValueLayout::NicheUnitResult => self.make_enum(0, Some(zero))?,
            _ => {
                let p = self.b.ins_cast(MirCastKind::Word, MirTy::HeapRef, w).map_err(mir)?;
                self.make_enum(0, Some(p))?
            }
        };
        out.push((self.cur, Val::V(first)));
        self.switch(hit);
        let payload = if layout == ValueLayout::NicheResult {
            let one = self.b.ins_const(MirConst::I64(1)).map_err(mir)?;
            self.b.ins_binop(MirBinOp::Xor, w, one).map_err(mir)?
        } else {
            w
        };
        let p = self.b.ins_cast(MirCastKind::Word, MirTy::HeapRef, payload).map_err(mir)?;
        let second = self.make_enum(1, Some(p))?;
        out.push((self.cur, Val::V(second)));
        match self.join_arms(out)? {
            Val::V(v) => Ok(v),
            _ => Err("boxed niche return".into()),
        }
    }

    /// How `lhs == rhs` on two two-word values compares their words, when
    /// it can: per variant of an enum pair (`[payload, tag]`), the
    /// payload's word type (`None` for a variant without one); for a
    /// product (`[a, b]`), its two words' types. `None` when an operand is
    /// not two words or a word is of a type compared another way (a heap
    /// word whose aggregate holds a `float`, which compares by IEEE).
    fn pair_eq_words(&self, id: HirId, lhs: HirId, rhs: HirId) -> Option<PairEqWords> {
        let tys = self.pair_tys(lhs)?;
        if self.pair_tys(rhs) != Some(tys) {
            return None;
        }
        let shaped = self.site(id).string.is_some();
        let plain = |t: MirTy| matches!(t, MirTy::I64 | MirTy::Bool | MirTy::F64) || (t == MirTy::HeapRef && !shaped);
        let ty = self.expr_ty(lhs)?;
        match self.cx.pair_variants(ty) {
            Some((_, variants)) => {
                if !plain(tys.0) || variants.iter().any(|&(_, p)| p.is_some_and(|p| !plain(p))) {
                    return None;
                }
                // A `()` payload is equal whatever word carries it.
                let units = self.cx.unit_tags(ty);
                Some(PairEqWords::Variants(variants.into_iter().map(|(t, p)| (t, p.filter(|_| units >> t & 1 == 0))).collect()))
            }
            None => (plain(tys.0) && plain(tys.1)).then_some(PairEqWords::Product(tys.0, tys.1)),
        }
    }

    /// `lhs == rhs` (`!=` when `ne`) on two two-word values as their words
    /// ([`Self::pair_eq_words`]): an enum pair's tags, then the variant's
    /// payload; a product's two words. No box made for either side.
    fn pair_eq(&mut self, id: HirId, lhs: HirId, rhs: HirId, ne: bool) -> Result<Val, Refusal> {
        let words = self.pair_eq_words(id, lhs, rhs).ok_or("pair eq")?;
        let ty = self.expr_ty(lhs).ok_or("pair eq type")?.clone();
        let (mut l, mut r) = (self.value(lhs)?, self.value(rhs)?);
        // A side that came out as its box (an element read) is unboxed, so
        // both compare as words: boxing the other side would compare a
        // `()` payload's word.
        if matches!((l, r), (Val::Pair(..), Val::V(_)) | (Val::V(_), Val::Pair(..))) {
            let (lo_ty, hi_ty) = self.pair_tys(lhs).expect("pair word types");
            for side in [&mut l, &mut r] {
                if let Val::V(v) = *side
                    && self.b.value_ty(v) == MirTy::HeapRef
                    && let Ok(p @ Val::Pair(..)) = self.unbox_word(&ty, v, lo_ty, hi_ty)
                {
                    *side = p;
                }
            }
        }
        let ((l0, l1), (r0, r1)) = match (l, r) {
            (Val::Never, _) | (_, Val::Never) => return Ok(Val::Never),
            (Val::Pair(a, b), Val::Pair(c, d)) => ((a, b), (c, d)),
            // A side that came out as its box: both compare boxed.
            (l, r) => {
                let boxed = |s: &mut Self, v: Val| -> Result<Option<ValueId>, Refusal> {
                    Ok(match v {
                        Val::Pair(a, b) => match s.box_pair_val(&ty, a, b)? {
                            Val::V(v) => Some(v),
                            _ => None,
                        },
                        Val::V(v) => Some(v),
                        _ => None,
                    })
                };
                let (Some(l), Some(r)) = (boxed(self, l)?, boxed(self, r)?) else {
                    return Ok(Val::Never);
                };
                return self.structural_eq(l, r, ne);
            }
        };
        let eq = match words {
            PairEqWords::Product(a, b) => {
                let first = self.word_eq(a, l0, r0)?;
                let second = self.word_eq(b, l1, r1)?;
                self.b.ins_bool_logic(MirBinOp::BitAnd, first, second).map_err(mir)?
            }
            PairEqWords::Variants(variants) => {
                let (check, differ) = (self.b.create_block(), self.b.create_block());
                let (lt, rt) = (self.fit(l1, MirTy::I64)?, self.fit(r1, MirTy::I64)?);
                let same = self.b.ins_cmp(MirCmpOp::Eq, lt, rt).map_err(mir)?;
                self.b.branch(same, check, differ).map_err(mir)?;
                let mut out = Vec::with_capacity(variants.len() + 1);
                self.switch(differ);
                let no = self.b.ins_const(MirConst::Bool(false)).map_err(mir)?;
                out.push((self.cur, Val::V(no)));
                self.switch(check);
                let last = variants.len().saturating_sub(1);
                for (k, &(tag, payload)) in variants.iter().enumerate() {
                    let next = (k < last).then(|| self.b.create_block());
                    if let Some(next) = next {
                        let (hit, t) = (self.b.create_block(), self.b.ins_const(MirConst::I64(i64::from(tag))).map_err(mir)?);
                        let is = self.b.ins_cmp(MirCmpOp::Eq, lt, t).map_err(mir)?;
                        self.b.branch(is, hit, next).map_err(mir)?;
                        self.switch(hit);
                    }
                    let eq = match payload {
                        Some(p) => {
                            let (a, b) = (self.fit(l0, p)?, self.fit(r0, p)?);
                            self.word_eq(p, a, b)?
                        }
                        None => self.b.ins_const(MirConst::Bool(true)).map_err(mir)?,
                    };
                    out.push((self.cur, Val::V(eq)));
                    if let Some(next) = next {
                        self.switch(next);
                    }
                }
                match self.join_arms(out)? {
                    Val::V(v) => v,
                    _ => return Err("pair eq join".into()),
                }
            }
        };
        Ok(Val::V(if ne { self.b.ins_not(eq).map_err(mir)? } else { eq }))
    }

    /// `a == b` on two words of type `ty`: a heap word's structural
    /// equality, a `float`'s IEEE compare, any other word's bits.
    fn word_eq(&mut self, ty: MirTy, a: ValueId, b: ValueId) -> Result<ValueId, Refusal> {
        match ty {
            MirTy::HeapRef => self.b.ins_str_eq(a, b, false).map_err(mir),
            MirTy::F64 => self.b.ins_cmp(MirCmpOp::Eq, a, b).map_err(mir),
            _ => {
                let a = self.fit(a, MirTy::I64)?;
                let b = self.fit(b, MirTy::I64)?;
                self.b.ins_cmp(MirCmpOp::Eq, a, b).map_err(mir)
            }
        }
    }

    /// `l == r` (`!=` when `ne`) on two niche words of `layout`, with no
    /// enum object made: unequal when their variants differ; for `None` /
    /// `Ok(())` (the zero word) equal; else their payloads' structural
    /// equality, as on the boxed objects.
    fn niche_eq(&mut self, layout: ValueLayout, l: ValueId, r: ValueId, ne: bool) -> Result<Val, Refusal> {
        let l = self.b.ins_cast(MirCastKind::Word, MirTy::I64, l).map_err(mir)?;
        let r = self.b.ins_cast(MirCastKind::Word, MirTy::I64, r).map_err(mir)?;
        let zero = self.b.ins_const(MirConst::I64(0)).map_err(mir)?;
        let one = self.b.ins_const(MirConst::I64(1)).map_err(mir)?;
        let (payloads, words) = (self.b.create_block(), self.b.create_block());
        let (lp, rp) = match layout {
            // `Ok(p)` is `p`, `Err(p)` is `p | 1`: the same low bit, then
            // the payloads with it cleared.
            ValueLayout::NicheResult => {
                let lb = self.b.ins_binop(MirBinOp::BitAnd, l, one).map_err(mir)?;
                let rb = self.b.ins_binop(MirBinOp::BitAnd, r, one).map_err(mir)?;
                let same = self.b.ins_cmp(MirCmpOp::Eq, lb, rb).map_err(mir)?;
                self.b.branch(same, payloads, words).map_err(mir)?;
                self.switch(payloads);
                let lp = self.b.ins_binop(MirBinOp::Xor, l, lb).map_err(mir)?;
                let rp = self.b.ins_binop(MirBinOp::Xor, r, rb).map_err(mir)?;
                (lp, rp)
            }
            // `None` / `Ok(())` is `0`: both non-zero compare payloads, else
            // the words themselves.
            ValueLayout::NicheOption | ValueLayout::NicheUnitResult => {
                let lz = self.b.ins_cmp(MirCmpOp::Eq, l, zero).map_err(mir)?;
                let rz = self.b.ins_cmp(MirCmpOp::Eq, r, zero).map_err(mir)?;
                let lz = self.b.ins_cast(MirCastKind::Word, MirTy::I64, lz).map_err(mir)?;
                let rz = self.b.ins_cast(MirCastKind::Word, MirTy::I64, rz).map_err(mir)?;
                let either = self.b.ins_binop(MirBinOp::BitOr, lz, rz).map_err(mir)?;
                let some = self.b.ins_cmp(MirCmpOp::Eq, either, zero).map_err(mir)?;
                self.b.branch(some, payloads, words).map_err(mir)?;
                self.switch(payloads);
                (l, r)
            }
            ValueLayout::Boxed => return Err("boxed niche layout".into()),
        };
        let lp = self.b.ins_cast(MirCastKind::Word, MirTy::HeapRef, lp).map_err(mir)?;
        let rp = self.b.ins_cast(MirCastKind::Word, MirTy::HeapRef, rp).map_err(mir)?;
        let eq = self.b.ins_str_eq(lp, rp, ne).map_err(mir)?;
        let mut out = vec![(self.cur, Val::V(eq))];
        self.switch(words);
        // A Result's variants differ here (unequal); an Option's at least
        // one side is `None`: equal when both are.
        let cmp = if ne { MirCmpOp::Ne } else { MirCmpOp::Eq };
        let same = match layout {
            ValueLayout::NicheResult => self.b.ins_cmp(cmp, one, zero).map_err(mir)?,
            _ => self.b.ins_cmp(cmp, l, r).map_err(mir)?,
        };
        out.push((self.cur, Val::V(same)));
        self.join_arms(out)
    }

    /// Niche-layout value `id` as the boxed enum object a generic class's
    /// open field holds: a read of another such field moves its object
    /// as is, any other value boxes.
    #[track_caller]
    fn boxed_niche_word(&mut self, id: HirId, layout: ValueLayout) -> Result<Option<ValueId>, Refusal> {
        if let HirKind::Field { base, .. } = self.hir.expr(id).kind
            && self.boxed_niche(id) == Some(layout)
            && let Some((slot, ty)) = self.heap_field(id)
        {
            let Some(obj) = self.boxed_word(base)? else {
                return Ok(None);
            };
            return Ok(Some(self.b.ins_product_load(obj, slot, ty).map_err(mir)?));
        }
        // So does a frame-slot class local's open niche field: its slot's box.
        if let Some(slot) = self.frame_field(id)
            && self.open_slot_niche(id).is_some_and(|w| Ok(w) == niche_word(layout))
        {
            return Ok(Some(match self.boxed_field(id, slot)? {
                Some((obj, index)) => self.b.ins_product_load(obj, index, MirTy::HeapRef).map_err(mir)?,
                None => self.b.use_local(LocalId(slot), MirTy::HeapRef).map_err(mir)?,
            }));
        }
        let Some(v) = self.word(id)? else {
            return Ok(None);
        };
        self.box_niche(layout, v).map(Some)
    }

    /// `MakeEnum tag` of at most one payload word.
    fn make_enum(&mut self, tag: u32, payload: Option<ValueId>) -> Result<ValueId, Refusal> {
        let obj = self.b.ins_alloc(MirAllocKind::Enum { tag }, payload.into_iter().collect()).map_err(mir)?;
        self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![obj]).map_err(mir)
    }

    /// `return` of a one-word function with no value: `0`, as HIR
    /// emission's `CONST 0; RETURN`.
    fn ret_zero(&mut self) -> Result<(), Refusal> {
        if self.ret_boxed_niche.is_some() {
            return self.ret(Val::Unit);
        }
        let ty = self.b.func().ret_ty.ok_or("return value")?;
        let z = self.b.ins_const(MirConst::I64(0)).map_err(mir)?;
        let z = self.fit(z, ty)?;
        self.ret_word(z)
    }

    /// Word types of pair local `local`, with its second slot.
    fn pair_local(&self, local: HirLocal) -> Option<(LocalId, LocalId, MirTy, MirTy)> {
        let (hi, lo_ty, hi_ty) = self.pair_local_tys(local)?;
        Some((self.slot(local).ok()?, LocalId(hi), lo_ty, hi_ty))
    }

    /// `v` as a word of type `ty`: itself, or the same word retyped (a heap
    /// payload moves through a pair as an opaque `i64`).
    fn fit(&mut self, v: ValueId, ty: MirTy) -> Result<ValueId, Refusal> {
        // The two niche words differ in their bits (an `Option`'s `None`
        // is `0`, a niche `Result`'s `Err` has the low bit set): never one
        // as the other.
        if is_niche(self.b.value_ty(v)) && is_niche(ty) && self.b.value_ty(v) != ty {
            return Err(format!("word {} as {ty}", self.b.value_ty(v)));
        }
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
            // A heap word's null: never read (`unassigned_reads`), and
            // skipped by the collector.
            ty if MirCastKind::Word.allows(MirTy::I64, ty) => {
                let zero = self.b.ins_const(MirConst::I64(0)).map_err(mir)?;
                return self.b.ins_cast(MirCastKind::Word, ty, zero).map_err(mir);
            }
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
            Val::V(v) if self.b.value_ty(v) == MirTy::HeapRef && let Some((lo_ty, hi_ty)) = self.pair_tys(id) => {
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

    /// `id`'s word, or its box when it is a two-word value.
    #[track_caller]
    fn word_or_boxed(&mut self, id: HirId) -> Result<Option<ValueId>, Refusal> {
        if self.pair_tys(id).is_some() {
            return self.boxed_word(id);
        }
        // A `()` is stored and passed as the empty tuple object (#1427).
        match self.value(id)? {
            Val::V(v) => Ok(Some(v)),
            Val::Never => Ok(None),
            Val::Unit => self.unit_object().map(Some),
            Val::Pair(..) => Err("pair operand".into()),
        }
    }

    /// The empty tuple object a `()` value is as a word.
    fn unit_object(&mut self) -> Result<ValueId, Refusal> {
        let obj = self.b.ins_alloc(MirAllocKind::TUPLE, Vec::new()).map_err(mir)?;
        self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![obj]).map_err(mir)
    }

    /// `id`'s word, a two-word value boxed to one, as HIR emission's
    /// conversion of a pair to a boxed local or return.
    #[track_caller]
    fn boxed_word(&mut self, id: HirId) -> Result<Option<ValueId>, Refusal> {
        // A value packed as an existential is its pack's one word.
        if self.cx.packs().programs.contains_key(&id.0) {
            return self.word(id);
        }
        // A two-word variant made boxed: one `MakeEnum`, not the pair then
        // its box.
        if let HirKind::Make { kind: MakeKind::Variant { enum_name, variant, tag: Some(tag), .. }, args } = &self.hir.expr(id).kind
            && self.pair_tys(id).is_some()
            && let Some(tys) = self.expr_ty(id).and_then(|t| self.cx.boxed_view(t, Some(variant)))
        {
            if let Some(ty) = self.expr_ty(id).cloned() {
                self.note_box(super::box_report::BoxKind::Variant, &ty);
            }
            let (enum_name, tag, args) = (enum_name.clone(), *tag, args.clone());
            return match self.make_boxed(&enum_name, tag, &tys, &args)? {
                Val::V(v) => Ok(Some(v)),
                Val::Never => Ok(None),
                _ => Err("make boxed".into()),
            };
        }
        // A host or call result that comes back boxed is its box already
        // (`Vec::pop`'s thunk returns `vec_pop`'s word): no unboxing to a
        // pair just to box it again.
        if self.host_pair(id) || self.boxed_call(id) {
            self.raw_host = true;
            let out = self.value(id);
            self.raw_host = false;
            return match out? {
                Val::V(v) if self.b.value_ty(v) == MirTy::HeapRef => Ok(Some(v)),
                Val::Never => Ok(None),
                _ => Err("boxed result".into()),
            };
        }
        // A two-word static is its box already, unless it keeps its two
        // words: then they are boxed here.
        if let HirKind::Global { .. } = self.hir.expr(id).kind
            && self.cx.static_tail(id).is_some()
        {
            let ty = self.expr_ty(id).ok_or("static type")?.clone();
            let Some((lo, hi)) = self.pair(id)? else {
                return Ok(None);
            };
            return match self.box_pair_val(&ty, lo, hi)? {
                Val::V(v) => Ok(Some(v)),
                _ => Err("static pair box".into()),
            };
        }
        if let HirKind::Global { .. } = self.hir.expr(id).kind
            && let Some(slot) = self.cx.static_slot(id)
            && self.pair_tys(id).is_some()
        {
            return Ok(Some(self.b.ins_load_static(slot, MirTy::HeapRef).map_err(mir)?));
        }
        match self.value(id)? {
            Val::V(v) => Ok(Some(v)),
            Val::Never => Ok(None),
            // A `()` is stored and passed as the empty tuple object.
            Val::Unit => self.unit_object().map(Some),
            Val::Pair(lo, hi) => {
                let ty = self.expr_ty(id).ok_or("pair operand")?.clone();
                match self.box_pair_val(&ty, lo, hi)? {
                    Val::V(v) => Ok(Some(v)),
                    _ => Err("pair operand".into()),
                }
            }
        }
    }

    /// A boxed word of two-word type `ty` as its pair.
    fn unbox_word(&mut self, ty: &Ty, v: ValueId, lo_ty: MirTy, hi_ty: MirTy) -> Result<Val, Refusal> {
        if let Ty::Tuple(items) = strip_readonly(ty) {
            if items.len() != 2 {
                return Err("unbox pair type".into());
            }
            let lo = self.b.ins_tuple_item(v, 0, lo_ty).map_err(mir)?;
            let hi = self.b.ins_tuple_item(v, 1, hi_ty).map_err(mir)?;
            return Ok(Val::Pair(lo, hi));
        }
        // A boxed numeric range: a typed object, start and end its fields;
        // a readonly class pair: an instance, its two fields (#1562).
        if crate::layout::two_word_range_kind(ty).is_some() || self.cx.pair_class(ty).is_some() {
            let lo = self.b.ins_product_load(v, 0, lo_ty).map_err(mir)?;
            let hi = self.b.ins_product_load(v, 1, hi_ty).map_err(mir)?;
            return Ok(Val::Pair(lo, hi));
        }
        if hi_ty != MirTy::I64 {
            return Err("unbox pair type".into());
        }
        let (_, variants) = self.cx.pair_variants(ty).ok_or("unbox pair type")?;
        // A `()` payload is `0` in the pair, not its box's word.
        let units = self.cx.unit_tags(ty);
        let variants: Vec<(u32, u32)> = variants
            .iter()
            .map(|&(t, w)| (t, if units >> t & 1 == 1 { UNIT_PAYLOAD } else { u32::from(w.is_some()) }))
            .collect();
        self.unbox_pair(v, lo_ty, &variants)
    }

    /// An element-wise operator: a
    /// static shape item by item (literal array items read in place, as
    /// they are reached), a dynamic array in a loop pushing each result.
    fn aggregate(&mut self, id: HirId, info: &crate::typechecking::AggregateArithInfo, lhs: HirId, rhs: Option<HirId>) -> Result<Val, Refusal> {
        use crate::typechecking::{AggregateMode as M, AggregateOp, AggregateShape as S, ScalarSide};
        let op = match info.op {
            AggregateOp::Add => Some(MirBinOp::Add),
            AggregateOp::Sub => Some(MirBinOp::Sub),
            AggregateOp::Mul => Some(MirBinOp::Mul),
            AggregateOp::Div => Some(MirBinOp::Div),
            AggregateOp::Mod => Some(MirBinOp::Rem),
            AggregateOp::Neg => None,
            AggregateOp::Pow => Some(MirBinOp::Pow),
        };
        let (shape, mode) = (info.kind.shape, info.kind.mode);
        let scalar_on = mode.scalar_on();
        // In a shared generic body the element is a type parameter: each
        // element is its one word and the operator its dictionary's method.
        let bound = self.site(id).bound_call.clone();
        // In a clone at a user type each element is that type's word and
        // the operator its instance method.
        let inst = self.site(id).elem_call.clone();
        let called = bound.is_some() || inst.is_some();
        let ty = match &inst {
            Some(_) => {
                let elem = std::iter::once(&lhs)
                    .chain(rhs.as_ref())
                    .filter_map(|&o| self.expr_ty(o))
                    .find_map(|t| crate::typechecking::aggregate_arith::homogeneous_aggregate_elem(strip_readonly(t)));
                // A nested aggregate's element is one word, a pair boxed,
                // as a tuple or an array holds it (#2544).
                let nested = |e: &Ty| matches!(strip_readonly(e), Ty::Tuple(_) | Ty::Array { .. }).then_some(MirTy::HeapRef);
                elem.and_then(|e| self.word_ty(&e).or_else(|| nested(&e))).ok_or("aggregate element word")?
            }
            None if bound.is_none() && info.kind.elem_is_float => MirTy::F64,
            None => MirTy::I64,
        };
        // Else every operand's elements (and a scalar) must be that
        // primitive word: no kernel and no int op runs on anything else.
        for &o in std::iter::once(&lhs).chain(rhs.as_ref()).filter(|_| !called) {
            let t = self.expr_ty(o).ok_or("aggregate operand")?;
            let elem = match strip_readonly(t) {
                Ty::Tuple(items) => {
                    let words = items.iter().map(scalar).collect::<Option<Vec<_>>>().ok_or("aggregate operand")?;
                    words.iter().all(|&w| w == ty).then_some(ty)
                }
                _ => array_elem(t).and_then(scalar).or_else(|| scalar(t)),
            };
            if elem != Some(ty) {
                return Err("aggregate operand".into());
            }
        }
        // The packed kernel: each operand one word (boxed), then the meta
        // word, as a matrix operator's.
        if !called && query::aggregate_packed(info).is_some() {
            let la = *self.site(id).linalg.as_ref().ok_or("packed aggregate")?;
            let args: Vec<HirId> = [lhs].into_iter().chain(rhs).collect();
            return self.linalg(id, la, &args);
        }
        // A `byte` element wraps modulo 256, as the scalar operator.
        let byte = info.kind.elem_is_byte;
        let apply = |l: &mut Self, a: ValueId, b: Option<ValueId>| -> Result<ValueId, Refusal> {
            if let Some(bound) = &bound {
                return l.bound_elem(id, bound, a, b);
            }
            if let Some((site, raw)) = &inst {
                return l.inst_elem(id, site, *raw, ty, a, b);
            }
            match (op, b) {
                (Some(MirBinOp::Pow), Some(b)) if byte => l.byte_pow(a, b),
                (Some(op), Some(b)) => {
                    let v = l.b.ins_binop(op, a, b).map_err(mir)?;
                    l.mask_byte(v, byte && matches!(op, MirBinOp::Add | MirBinOp::Sub | MirBinOp::Mul))
                }
                (None, None) => {
                    let v = l.b.ins_neg(a).map_err(mir)?;
                    l.mask_byte(v, byte)
                }
                _ => Err("aggregate operands".into()),
            }
        };
        let rhs = match mode {
            M::Neg => None,
            M::Zip | M::Broadcast(_) => Some(rhs.ok_or("aggregate rhs")?),
        };
        let Some(n) = shape.static_len() else {
            return self.aggregate_dynamic(lhs, rhs, mode, ty, apply);
        };
        // A static shape: the operands run left to right. An array's
        // elements are read where they are (its literal items in order, with
        // no array made), a tuple is a value first.
        let array = !shape.is_tuple();
        let open = called;
        let side = |l: &mut Self, operand: HirId, on: ScalarSide| -> Result<Option<AggSide>, Refusal> {
            if scalar_on == Some(on) {
                return Ok(l.agg_scalar(operand, ty)?.map(AggSide::Scalar));
            }
            Ok(l.agg_operand(operand, array)?.map(AggSide::Elems))
        };
        let Some(a) = side(self, lhs, ScalarSide::Left)? else {
            return Ok(Val::Never);
        };
        let b = match rhs {
            Some(r) => match side(self, r, ScalarSide::Right)? {
                Some(b) => Some(b),
                None => return Ok(Val::Never),
            },
            None => None,
        };
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let x = self.agg_side(&a, i, ty)?;
            let y = match &b {
                Some(b) => Some(self.agg_side(b, i, ty)?),
                None => None,
            };
            out.push(apply(self, x, y)?);
        }
        if let (S::Tuple(_), Some(_)) = (shape, self.pair_tys(id)) {
            return match out[..] {
                [a, b] => Ok(Val::Pair(a, b)),
                _ => Err("aggregate pair".into()),
            };
        }
        let kind = if array { MirAllocKind::Array } else { MirAllocKind::TUPLE };
        // A type parameter's or a user type's words may be pointers:
        // stamped as a literal of the result type is.
        let kinds = match self.expr_ty(id).filter(|_| open).map(strip_readonly) {
            Some(Ty::Tuple(items)) => common::pack_word_kinds(items.iter().map(|t| self.cx.word_kind(t))),
            Some(t) => array_elem(t).map_or(0, |e| self.cx.array_stamp(e)),
            None => 0,
        };
        let obj = self.b.ins_alloc_kinds(kind, out, kinds).map_err(mir)?;
        Ok(Val::V(self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![obj]).map_err(mir)?))
    }

    /// One element of an element-wise operator at a user type: the
    /// instance method's plain call on the operand words (a heap carrier's
    /// word where the instance does not take them raw), then its
    /// dictionaries, as [`Self::op_call`] on one value.
    fn inst_elem(&mut self, id: HirId, site: &HirCallSite, raw: bool, ret: MirTy, a: ValueId, b: Option<ValueId>) -> Result<ValueId, Refusal> {
        if site.pair_ret || site.coro || site.pair_args.iter().any(|&pair| pair) {
            return Err("element call shape".into());
        }
        let mut words = Vec::with_capacity(2);
        for v in std::iter::once(a).chain(b) {
            if raw {
                words.push(v);
            } else if self.b.value_ty(v) == MirTy::HeapRef {
                words.push(self.b.ins_word(v).map_err(mir)?);
            } else {
                return Err("element call operand".into());
            }
        }
        words.extend(self.dict_values(&site.dicts)?);
        let abi = CallAbi {
            params: words.iter().map(|&v| self.b.value_ty(v)).collect(),
            ret,
            ret_hi: None,
        };
        let (dest, _) = self.at_expr(id, |l| l.b.ins_call(site.target, words, &abi).map_err(mir))?;
        Ok(dest)
    }

    /// One element of an element-wise operator on a bound type parameter:
    /// the operand words, the dictionary parameter (and the method's own
    /// bounds' dictionaries), then the code pointer in the method's slot
    /// through `CallIndirect`, as [`Self::bound_call`] on one value.
    fn bound_elem(&mut self, id: HirId, bound: &HirBoundCall, a: ValueId, b: Option<ValueId>) -> Result<ValueId, Refusal> {
        let dict = *self.dict_params.get(&bound.dict).ok_or("dictionary parameter")?;
        let mut words: Vec<ValueId> = std::iter::once(a).chain(b).collect();
        words.push(dict);
        words.extend(self.dict_values(&bound.dicts)?);
        let callee = self.b.ins_tuple_item(dict, bound.method, MirTy::I64).map_err(mir)?;
        self.at_expr(id, |l| l.b.ins_call_indirect(callee, words, MirTy::I64).map_err(mir))
    }

    /// An element-wise negation or broadcast over an array of dynamic
    /// length: a loop pushing each result.
    fn aggregate_dynamic(
        &mut self,
        lhs: HirId,
        rhs: Option<HirId>,
        mode: crate::typechecking::AggregateMode,
        ty: MirTy,
        apply: impl Fn(&mut Self, ValueId, Option<ValueId>) -> Result<ValueId, Refusal>,
    ) -> Result<Val, Refusal> {
        use crate::typechecking::{AggregateMode as M, ScalarSide};
        let scalar_on = match mode {
            M::Zip => return Err("aggregate shape".into()),
            M::Broadcast(side) => Some(side),
            M::Neg => None,
        };
        // Both operands first, in order, as emission's temps; a two-word
        // scalar boxed, as an element is.
        let operand = |l: &mut Self, id: HirId, on: ScalarSide| match l.pair_tys(id) {
            Some(_) if scalar_on == Some(on) => l.boxed_word(id),
            _ => l.word(id),
        };
        let Some(a) = operand(self, lhs, ScalarSide::Left)? else {
            return Ok(Val::Never);
        };
        let b = match rhs {
            Some(r) => match operand(self, r, ScalarSide::Right)? {
                Some(v) => Some(v),
                None => return Ok(Val::Never),
            },
            None => None,
        };
        let (arr, k) = match scalar_on {
            Some(ScalarSide::Left) => (b.ok_or("aggregate rhs")?, Some(a)),
            Some(ScalarSide::Right) => (a, Some(b.ok_or("aggregate rhs")?)),
            None => (a, None),
        };
        if self.b.value_ty(arr) != MirTy::HeapRef || k.is_some_and(|k| self.b.value_ty(k) != ty) {
            return Err("aggregate operand".into());
        }
        // `out` and the index live in temps across the loop.
        let len = self.b.ins_array_len(arr).map_err(mir)?;
        let empty = self.b.ins_alloc(MirAllocKind::Array, Vec::new()).map_err(mir)?;
        let empty = self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![empty]).map_err(mir)?;
        let out = self.temp();
        self.b.def_local(out, empty).map_err(mir)?;
        let i = self.temp();
        let zero = self.b.ins_const(MirConst::I64(0)).map_err(mir)?;
        self.b.def_local(i, zero).map_err(mir)?;
        let head = self.b.create_block();
        let body = self.b.create_block();
        let exit = self.b.create_block();
        self.jump(head)?;
        self.switch(head);
        let cur = self.b.use_local(i, MirTy::I64).map_err(mir)?;
        let more = self.b.ins_cmp(MirCmpOp::Lt, cur, len).map_err(mir)?;
        self.b.branch(more, body, exit).map_err(mir)?;
        self.switch(body);
        let cur = self.b.use_local(i, MirTy::I64).map_err(mir)?;
        let acc = self.b.use_local(out, MirTy::HeapRef).map_err(mir)?;
        let e = self.b.ins_index(arr, cur, ty, false).map_err(mir)?;
        let v = match (scalar_on, k) {
            (Some(ScalarSide::Left), Some(k)) => apply(self, k, Some(e))?,
            (_, k) => apply(self, e, k)?,
        };
        let pushed = self.b.ins_array_push(acc, v).map_err(mir)?;
        let pushed = self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![pushed]).map_err(mir)?;
        self.b.def_local(out, pushed).map_err(mir)?;
        let one = self.b.ins_const(MirConst::I64(1)).map_err(mir)?;
        let next = self.b.ins_binop(MirBinOp::Add, cur, one).map_err(mir)?;
        self.b.def_local(i, next).map_err(mir)?;
        self.jump(head)?;
        self.switch(exit);
        Ok(Val::V(self.b.use_local(out, MirTy::HeapRef).map_err(mir)?))
    }

    /// How an aggregate operand's elements are read: literal array items
    /// and stack array slots (only when `items`) as values, read here in
    /// order; a pair; or a heap word (a scalar for a broadcast's other side).
    fn agg_operand(&mut self, id: HirId, items: bool) -> Result<Option<AggSrc>, Refusal> {
        if items && let Some(src) = query::aggregate_src(self.hir, &self.stack_lens(id), id) {
            match src {
                query::AggregateSrc::Items(items) => {
                    let mut elems = Vec::with_capacity(items.len());
                    for item in items {
                        let Some(v) = self.word(item)? else {
                            return Ok(None);
                        };
                        elems.push(v);
                    }
                    return Ok(Some(AggSrc::Elems(elems)));
                }
                query::AggregateSrc::Slots(local) if self.boxed(id).is_none() => {
                    let (base, n, ty) = self.stack(local.0).expect("stack array");
                    let slots = (0..n as u32).map(|i| self.b.use_local(LocalId(base + i), ty).map_err(mir));
                    return Ok(Some(AggSrc::Elems(slots.collect::<Result<_, _>>()?)));
                }
                _ => {}
            }
        }
        if self.pair_tys(id).is_some() {
            return Ok(self.pair(id)?.map(|(a, b)| AggSrc::Pair(a, b)));
        }
        Ok(self.word(id)?.map(AggSrc::Word))
    }

    /// Element `i` of an aggregate operand, of word type `ty`.
    fn agg_elem(&mut self, src: &AggSrc, i: usize, ty: MirTy) -> Result<ValueId, Refusal> {
        let v = match src {
            AggSrc::Elems(elems) => *elems.get(i).ok_or("aggregate item")?,
            AggSrc::Pair(a, b) => match i {
                0 => *a,
                1 => *b,
                _ => return Err("aggregate item".into()),
            },
            AggSrc::Word(w) if self.b.value_ty(*w) == MirTy::HeapRef => {
                let c = self.b.ins_const(MirConst::I64(i as i64)).map_err(mir)?;
                self.b.ins_index(*w, c, ty, false).map_err(mir)?
            }
            _ => return Err("aggregate item".into()),
        };
        if self.b.value_ty(v) != ty {
            return Err("aggregate item type".into());
        }
        Ok(v)
    }

    /// The scalar side of a broadcast: one word of the element's type, a
    /// two-word value boxed as an element is.
    fn agg_scalar(&mut self, operand: HirId, ty: MirTy) -> Result<Option<ValueId>, Refusal> {
        let word = match self.pair_tys(operand) {
            Some(_) => self.boxed_word(operand)?,
            None => self.word(operand)?,
        };
        match word {
            Some(w) if self.b.value_ty(w) != ty => Err("aggregate scalar".into()),
            word => Ok(word),
        }
    }

    /// What one side of an element-wise operator gives element `i`.
    fn agg_side(&mut self, side: &AggSide, i: usize, ty: MirTy) -> Result<ValueId, Refusal> {
        match side {
            AggSide::Scalar(v) => Ok(*v),
            AggSide::Elems(src) => self.agg_elem(src, i, ty),
        }
    }

    /// A boxed tuple: its fields in order, then `Alloc`.
    fn make_tuple(&mut self, args: &[HirId]) -> Result<Val, Refusal> {
        let mut words = Vec::with_capacity(args.len());
        for &arg in args {
            // A two-word item is stored boxed.
            let Some(v) = self.word_or_boxed(arg)? else {
                return Ok(Val::Never);
            };
            words.push(v);
        }
        // A `()` item is the empty tuple object, a pointer, as an enum's
        // `()` payload (#1427).
        let kinds = common::pack_word_kinds(args.iter().map(|&a| self.expr_ty(a).map_or(common::WORD_UNKNOWN, |t| self.cx.word_kind(t))));
        let obj = self.b.ins_alloc_kinds(MirAllocKind::TUPLE, words, kinds).map_err(mir)?;
        Ok(Val::V(self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![obj]).map_err(mir)?))
    }

    /// `a..b` over a shared generic body's open `T` (#1230): the typed
    /// range object a boxed `Range<int>` is, its bounds the unboxed words.
    fn make_open_range(&mut self, id: HirId, args: &[HirId]) -> Result<Val, Refusal> {
        use crate::layout as rl;
        let [a, b] = args else {
            return Err("make range".into());
        };
        let (_, inclusive) = self.expr_ty(id).and_then(crate::typechecking::ty::range_app).ok_or("make range type")?;
        let Some(lo) = self.word(*a)? else {
            return Ok(Val::Never);
        };
        let Some(hi) = self.word(*b)? else {
            return Ok(Val::Never);
        };
        let (lo, hi) = (self.fit(lo, MirTy::I64)?, self.fit(hi, MirTy::I64)?);
        let type_id = rl::range_heap_type_id(inclusive);
        let obj = self.b.ins_alloc(MirAllocKind::Object { type_id, nfields: 2 }, Vec::new()).map_err(mir)?;
        let obj = self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![obj]).map_err(mir)?;
        self.b.ins_heap_field_store(obj, lo, 0).map_err(mir)?;
        self.b.ins_heap_field_store(obj, hi, 1).map_err(mir)?;
        Ok(Val::V(obj))
    }

    /// A pair of type `ty` boxed: a two-field tuple, or an enum value.
    /// Record a box of a value of type `ty` (`coil dissect --boxes`).
    #[track_caller]
    fn note_box(&self, kind: super::box_report::BoxKind, ty: &Ty) {
        super::box_report::note(&self.b.func().name, kind, || crate::typechecking::pretty::format_ty_for_diag(&Default::default(), ty));
    }

    #[track_caller]
    fn box_pair_val(&mut self, ty: &Ty, lo: ValueId, hi: ValueId) -> Result<Val, Refusal> {
        self.note_box(super::box_report::BoxKind::Pair, ty);
        if let Ty::Tuple(items) = strip_readonly(ty) {
            if items.len() != 2 {
                return Err("box pair type".into());
            }
            let obj = self.b.ins_alloc(MirAllocKind::TUPLE, vec![lo, hi]).map_err(mir)?;
            return Ok(Val::V(self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![obj]).map_err(mir)?));
        }
        // A numeric range: its typed object, start and end its fields.
        if let Some(crate::layout::PairKind::Range { inclusive }) = crate::layout::two_word_range_kind(ty) {
            let type_id = crate::layout::range_heap_type_id(inclusive);
            let obj = self.b.ins_alloc(MirAllocKind::Object { type_id, nfields: 2 }, Vec::new()).map_err(mir)?;
            let obj = self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![obj]).map_err(mir)?;
            self.b.ins_heap_field_store(obj, lo, 0).map_err(mir)?;
            self.b.ins_heap_field_store(obj, hi, 1).map_err(mir)?;
            return Ok(Val::V(obj));
        }
        // A readonly class pair: an instance of the class (#1562).
        if let Some(type_id) = self.cx.pair_class(ty) {
            let obj = self.b.ins_alloc(MirAllocKind::Object { type_id, nfields: 2 }, Vec::new()).map_err(mir)?;
            let obj = self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![obj]).map_err(mir)?;
            self.b.ins_heap_field_store(obj, lo, 0).map_err(mir)?;
            self.b.ins_heap_field_store(obj, hi, 1).map_err(mir)?;
            return Ok(Val::V(obj));
        }
        // The same words boxed before, where that box dominates and in the
        // same loop (a loop could rewrite the local after this point and
        // come back here without passing the box): its object.
        let key = |l: &Self, v: ValueId| l.b.read_origin(v).map_or(PairWord::Value(l.b.resolved(v)), |(local, n)| PairWord::Read(local, n));
        let (lo_key, hi_key) = (key(self, lo), key(self, hi));
        let in_loop = self.loops.last().map(|l| l.cont);
        let found = self
            .pair_boxes
            .iter()
            .find(|p| p.lo == lo_key && p.hi == hi_key && p.in_loop == in_loop && &p.ty == ty && self.b.dominates_current(p.block))
            .map(|p| p.obj);
        if let Some(obj) = found {
            return Ok(Val::V(self.b.resolved(obj)));
        }
        let boxed = self.box_pair(ty, lo, hi)?;
        if let Val::V(obj) = boxed {
            self.pair_boxes.push(PairBox { lo: lo_key, hi: hi_key, ty: ty.clone(), in_loop, block: self.cur, obj });
        }
        Ok(boxed)
    }

    /// `id`'s word; `Never` is passed up as `None`. A
    /// readonly class kept as a pair is one word as its instance (#1562).
    #[track_caller]
    fn word(&mut self, id: HirId) -> Result<Option<ValueId>, Refusal> {
        match self.value(id)? {
            Val::V(v) => Ok(Some(v)),
            Val::Never => Ok(None),
            // A `()` is stored and passed as the empty tuple object.
            Val::Unit => self.unit_object().map(Some),
            Val::Pair(lo, hi) if let Some(ty) = self.expr_ty(id).filter(|t| self.cx.pair_class(t).is_some()).cloned() => {
                match self.box_pair_val(&ty, lo, hi)? {
                    Val::V(v) => Ok(Some(v)),
                    _ => Err("pair operand".into()),
                }
            }
            Val::Pair(..) => Err("pair operand".into()),
        }
    }

    /// A block's statements, then its tail's value.
    fn block_body(&mut self, id: HirId, stmts: &[HirId], tail: Option<HirId>, returned: bool) -> Result<Val, Refusal> {
        for &stmt in stmts {
            if !self.stmt(stmt)? {
                return Ok(Val::Never);
            }
        }
        // A `()` block's host-call tail is dropped (`main`'s
        // `write(..)`): its result stays raw.
        if let Some(t) = tail
            && self.expr_ty(id).is_some_and(is_unit)
            && (self.host_pair(t) || self.boxed_call(t))
        {
            self.raw_host = true;
        }
        match &tail {
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

    /// Lower `id` for its effect.
    fn effect(&mut self, id: HirId) -> Result<bool, Refusal> {
        // Walks recurse through here once per level of the tree.
        common::deep_stack(|| self.in_scope(id, |l| l.effect_level(id)))
    }

    /// `f` lowering node `id`, located in `id`'s inline scope when it is not
    /// the one of the code around it: an inlined callee's result, lowered
    /// inside the caller's statement, is at the callee's `return` (#2578).
    fn in_scope<T>(&mut self, id: HirId, f: impl FnOnce(&mut Self) -> Result<T, Refusal>) -> Result<T, Refusal> {
        let k = self.hir.expr(id).scope;
        let outer = self.b.pending_loc;
        let Some(file) = self.cx.file().filter(|_| k != 0 && outer.is_known()) else { return f(self) };
        let scope = self.program_scope(file, k);
        if scope == outer.scope {
            return f(self);
        }
        let (start, end) = self.hir.inline_scopes[k as usize - 1].ret;
        let file = self.scope_file(file, k);
        if (start, end) != crate::hir::synth::SPAN {
            let (start, end) = crate::variadic::written_span(start, end);
            self.b.pending_loc = DebugLoc { file, start_byte: start as u32, end_byte: end.max(start + 1) as u32, scope };
        }
        let out = f(self);
        self.b.pending_loc = outer;
        out
    }

    fn effect_level(&mut self, id: HirId) -> Result<bool, Refusal> {
        // A statement `yield` leaves nothing: a send lands only where a
        // receiving `yield` takes it.
        if let HirKind::Yield { value, from, .. } = self.hir.expr(id).kind {
            return Ok(!matches!(self.yield_(id, value, from, false)?, Val::Never));
        }
        // A statement `resume` reads nothing: no zero, no panic, no pair
        // unpacked from the completing resume's word (#2717).
        if let HirKind::Resume { handle, value, .. } = self.hir.expr(id).kind {
            return Ok(!matches!(self.resume(id, handle, value, false)?, Val::Never));
        }
        // A call's two-word result nobody reads stays the boxed word it
        // comes back as: no unboxing to a pair just to drop it. A host's
        // comes back as its two words, no box made.
        self.raw_host = self.boxed_call(id);
        self.drop_value(id);
        let out = self.value(id);
        self.raw_host = false;
        Ok(!matches!(out?, Val::Never))
    }

    /// Nothing reads `id`'s value: when it is a `match` (or a block ending
    /// in one), that `match`'s arms drop their values.
    fn drop_value(&mut self, mut id: HirId) {
        while let HirKind::Block { tail: Some(t), .. } = self.hir.expr(id).kind {
            id = t;
        }
        match self.hir.expr(id).kind {
            HirKind::Match { .. } => {
                self.stmt_matches.insert(id);
            }
            HirKind::Resume { .. } | HirKind::Yield { .. } => {
                self.unread.insert(id);
            }
            HirKind::If { then, els, .. } => {
                self.drop_value(then);
                if let Some(e) = els {
                    self.drop_value(e);
                }
            }
            _ => {}
        }
    }

    /// A host call whose result is a boxed enum unboxed to a pair.
    fn host_pair(&self, id: HirId) -> bool {
        matches!(self.hir.expr(id).kind, HirKind::Call { .. })
            && self.host_call(id).is_some_and(|h| h.2.is_some())
            && self.pair_tys(id).is_some()
    }

    /// A [`Self::host_pair`] call that comes back as its two words
    /// (`HOST_ENUM_LAYOUT_PAIR`) when read as a value: a scalar payload.
    fn host_two_words(&self, id: HirId) -> bool {
        self.host_pair(id) && self.pair_tys(id).is_some_and(|(lo, _)| matches!(lo, MirTy::I64 | MirTy::F64))
    }

    /// A host call whose result is a boxed enum kept as its object: no
    /// word or pair of its own.
    fn host_boxed(&self, id: HirId) -> bool {
        // Or an `if` choosing between such calls (a fork-join worker's spawn
        // of itself or of the sequential arm).
        if let HirKind::If { then, els: Some(els), .. } = self.hir.expr(id).kind {
            let tail = |b: HirId| match &self.hir.expr(b).kind {
                HirKind::Block { stmts, tail: Some(t) } if stmts.is_empty() => *t,
                _ => b,
            };
            return self.host_boxed(tail(then)) && self.host_boxed(tail(els));
        }
        matches!(self.hir.expr(id).kind, HirKind::Call { .. })
            && self.host_call(id).is_some()
            && self.pair_tys(id).is_none()
            && self.expr_ty(id).is_some_and(|t| self.word_ty(t).is_none() && self.cx.boxed_view(t, None).is_some())
    }

    /// A call whose two-word result comes back boxed (`Vec::pop`'s thunk,
    /// any call through a function value).
    fn boxed_call(&self, id: HirId) -> bool {
        (self.site(id).call.as_ref().is_some_and(|site| !site.pair_ret) || (self.site(id).value_call && !self.site(id).value_pair_call.is_some()))
            && self.pair_tys(id).is_some()
    }

    /// Lower `id` for its value, packed as a bare-class existential where
    /// HIR emission packs it.
    fn value(&mut self, id: HirId) -> Result<Val, Refusal> {
        // Walks recurse through here once per level of the tree.
        common::deep_stack(|| self.in_scope(id, |l| l.value_level(id)))
    }

    fn value_level(&mut self, id: HirId) -> Result<Val, Refusal> {
        let mut val = self.value_unpacked(id)?;
        // A `-> ()` function stored where a shared body reads its result
        // as a type parameter: wrapped to return the empty tuple object.
        if let (Val::V(v), Some(&(target, operand))) = (val, self.cx.packs().unit_fns.get(&id.0)) {
            if self.b.value_ty(v) != MirTy::HeapRef {
                return Err("unit function adapter".into());
            }
            let f = self.at_expr(id, |l| l.b.ins_alloc(MirAllocKind::Fn { target, operand, mask: 0 }, vec![v]).map_err(mir))?;
            val = Val::V(self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![f]).map_err(mir)?);
        }
        // A lane array leaving for one-word code: boxed in place.
        if self.lane_escape(id)
            && let (Val::V(v), Some(lane)) = (val, self.lane(id))
        {
            if let Some(ty) = self.expr_ty(id).cloned() {
                self.note_box(super::box_report::BoxKind::LaneArray, &ty);
            }
            let out = self.b.ins_lanes_to_words(v, lane.shape).map_err(mir)?;
            val = Val::V(self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![out]).map_err(mir)?);
        }
        let Some(ops) = self.cx.packs().programs.get(&id.0) else {
            return Ok(val);
        };
        let v = match val {
            Val::V(v) => v,
            Val::Never => return Ok(Val::Never),
            // A two-word enum packs its boxed form, as the instance's
            // methods take it.
            Val::Pair(lo, hi) => {
                let ty = self.expr_ty(id).ok_or("existential pack type")?.clone();
                match self.box_pair_val(&ty, lo, hi)? {
                    Val::V(v) => v,
                    _ => return Err("existential pack value".into()),
                }
            }
            _ => return Err("existential pack value".into()),
        };
        let mut out = self.dict_program(vec![v], ops)?;
        let (Some(pack), true) = (out.pop(), out.is_empty()) else {
            return Err("existential pack program".into());
        };
        self.packed.insert(id.0);
        Ok(Val::V(pack))
    }

    fn value_unpacked(&mut self, id: HirId) -> Result<Val, Refusal> {
        if let Some(&v) = self.preset.get(&id.0) {
            return Ok(v);
        }
        let e = self.hir.expr(id);
        let returned = std::mem::take(&mut self.ret_tail);
        if let Some(par) = self.site(id).par_loop.as_ref() {
            return self.par_loop(id, par);
        }
        match &e.kind {
            HirKind::Lit(lit) => {
                let c = match lit {
                    // A folded `!0` / `!1`: an int `0` / `1` typed `bool` is
                    // that bool's word.
                    Lit::Int(n @ (0 | 1)) if self.expr_ty(id).and_then(scalar) == Some(MirTy::Bool) => MirConst::Bool(*n == 1),
                    Lit::Int(n) => MirConst::I64(*n),
                    Lit::Float(f) => MirConst::F64(f.to_bits()),
                    Lit::Bool(v) => MirConst::Bool(*v),
                    Lit::Unit => return Ok(Val::Unit),
                    Lit::Str(raw) if self.expr_ty(id).and_then(classify::primitive) == Some(crate::typechecking::ty::BYTE) => {
                        MirConst::I64(i64::from(query::byte_literal(raw).ok_or("byte literal")?))
                    }
                    Lit::Str(_) => match self.site(id).string.as_ref() {
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
            // A generic function's value made where a local bound from it
            // is passed to a `forall` parameter (#2637).
            HirKind::Local(_) if let Some(f) = self.site(id).fn_value.as_ref() => self.fn_value(id, &f.clone()),
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
            // A `()` local (a `?` on `Result<(), E>`) holds nothing to read.
            HirKind::Local(local) if self.unit_local(*local) => Ok(Val::Unit),
            HirKind::Local(local) => {
                let ty = self.local_ty(*local)?;
                let slot = self.slot(*local)?;
                Ok(Val::V(self.b.use_local(slot, ty).map_err(mir)?))
            }
            HirKind::Global { .. } | HirKind::Lambda { .. } if let Some(f) = self.site(id).fn_value.as_ref() => self.fn_value(id, &f.clone()),
            HirKind::Global { .. } => self.global(id),
            // `a + b` on strings is `format("%s%s", a, b)`; a chain `a + b +
            // c` is one `format("%s%s%s", a, b, c)`.
            HirKind::Bin { op: BinOp::StrConcat, .. } if let Some(&idx) = self.site(id).format.as_ref() => {
                let leaves = self.hir.concat_leaves(id);
                self.format(id, idx, &leaves)
            }
            HirKind::Bin { lhs, rhs, .. } if let Some(&la) = self.site(id).linalg.as_ref() => self.linalg(id, la, &[*lhs, *rhs]),
            HirKind::Un { operand, .. } if let Some(&la) = self.site(id).linalg.as_ref() => self.linalg(id, la, &[*operand]),
            HirKind::Call { args, .. } if let Some(&la) = self.site(id).linalg.as_ref() => self.linalg(id, la, args),
            HirKind::Call { args, .. } if let Some(&(tuple, float, byte)) = self.site(id).cross.as_ref() => self.cross(id, (tuple, float, byte), args),
            HirKind::Call { args, .. } if let Some(kind) = self.site(id).unrolled_la.as_ref() => self.unrolled_la(id, kind.clone(), args),
            HirKind::Bin { lhs, rhs, .. } if let Some(kind) = self.site(id).unrolled_la.as_ref() => self.unrolled_la(id, kind.clone(), &[*lhs, *rhs]),
            HirKind::Un { operand, .. } if let Some(kind) = self.site(id).unrolled_la.as_ref() => self.unrolled_la(id, kind.clone(), &[*operand]),
            HirKind::Bin { lhs, rhs, .. } if let Some(info) = self.cx.aggregate(id) => self.aggregate(id, &info.clone(), *lhs, Some(*rhs)),
            HirKind::Un { operand, .. } if let Some(info) = self.cx.aggregate(id) => self.aggregate(id, &info.clone(), *operand, None),
            // A bound type parameter's operator: its dictionary method.
            HirKind::Bin { lhs, rhs, .. } if let Some(bound) = self.site(id).bound_call.as_ref() => self.bound_call(id, &[*lhs, *rhs], bound),
            HirKind::Un { operand, .. } if let Some(bound) = self.site(id).bound_call.as_ref() => self.bound_call(id, &[*operand], bound),
            // An operator on an instance taking its operands raw: a plain call.
            HirKind::Bin { lhs, rhs, .. } if self.site(id).call.is_some() => self.call(id, &[*lhs, *rhs]),
            HirKind::Un { operand, .. } if self.site(id).call.is_some() => self.call(id, &[*operand]),
            HirKind::Bin { lhs, rhs, .. } if let Some(call) = self.site(id).op_call.as_ref() => self.op_call(id, *lhs, *rhs, &call.clone()),
            HirKind::Bin { op: BinOp::Overloaded(sym), lhs, rhs } if let Some(&slot) = self.site(id).existential_call.as_ref() => {
                self.existential_cmp(id, sym, *lhs, *rhs, slot)
            }
            HirKind::Bin { op: BinOp::Overloaded(sym), lhs, rhs } if self.site(id).op_prim => self.op_prim(id, sym, *lhs, *rhs),
            HirKind::Bin { op, lhs, rhs } => self.bin(id, *op, *lhs, *rhs),
            HirKind::Call { args, .. } if let Some(f) = self.site(id).fn_value.as_ref() => self.partial(id, &f.clone(), args),
            HirKind::Call { args, .. } if let (Some(fixed), [arg]) = (self.cx.len_of(id), args.as_slice()) => self.len(*arg, fixed),
            HirKind::Call { args, .. } if self.cx.push(id) => self.push(args),
            HirKind::Call { args, .. } if self.site(id).matrix_wrap && args.len() == 1 => self.value(args[0]),
            HirKind::Call { args, .. } if let Some(&idx) = self.site(id).format.as_ref() => self.format(id, idx, args.get(1..).unwrap_or_default()),
            HirKind::Call { args, .. } if self.host_call(id).is_some() => self.host(id, args),
            HirKind::Call { .. } if let Some(site) = self.site(id).ffi.as_ref() => self.ffi(id, &site.clone()),
            HirKind::Call { callee: Callee::Value(f), args } if self.site(id).value_call => self.call_value(id, *f, args),
            HirKind::Call { callee: Callee::Value(f), args } if let Some(p) = self.site(id).poly_call.as_ref() => self.call_poly(id, *f, args, &p.clone()),
            HirKind::Call { args, .. } if let Some(fork) = self.site(id).par_fork.as_ref() => self.par_fork(id, args, &fork.clone()),
            HirKind::Call { args, .. } if let Some(bound) = self.site(id).bound_call.as_ref() => self.bound_call(id, args, bound),
            HirKind::Call { args, .. } if let Some(&slot) = self.site(id).existential_call.as_ref() => self.existential_call(id, args, slot),
            HirKind::Call { args, .. } => self.call(id, args),
            HirKind::Resume { handle, value, .. } => {
                let read = !self.unread.remove(&id);
                self.resume(id, *handle, *value, read)
            }
            HirKind::Yield { value, from, .. } => {
                let read = !self.unread.remove(&id);
                self.yield_(id, *value, *from, read)
            }
            // Arm the flag; the end of the block, or a `return`, `break`
            // or `continue` leaving it, runs the body if armed.
            HirKind::Defer { body, .. } => {
                let flag = *self.defer_flags.get(&id.0).ok_or("defer")?;
                let on = self.b.ins_const(MirConst::Bool(true)).map_err(mir)?;
                self.b.def_local(flag, on).map_err(mir)?;
                self.defers.push(Open::Defer(flag, *body));
                self.lowered_defers.push((flag, *body));
                Ok(Val::Unit)
            }
            // `PRINT` of a string (the test-runner `main`'s failure line).
            HirKind::Builtin { op: Builtin::Print, args } if let [msg] = args.as_slice() => {
                let Some(s) = self.word(*msg)? else {
                    return Ok(Val::Never);
                };
                if self.b.value_ty(s) != MirTy::HeapRef {
                    return Err("print operand".into());
                }
                self.at_expr(id, |l| l.b.ins_print(s).map_err(mir))?;
                Ok(Val::Unit)
            }
            // An early free (`FreeEarly`) of a local's object.
            HirKind::Builtin { op: Builtin::FreeEarly { deep }, args } if let [local] = args.as_slice() => {
                let Some(v) = self.word(*local)? else {
                    return Ok(Val::Never);
                };
                // A word that is no reference (a scalar niche) has nothing
                // to free; a pointer niche frees the object it points at.
                if !matches!(self.b.value_ty(v), MirTy::HeapRef | MirTy::NicheOpt | MirTy::NicheRes | MirTy::I64) {
                    return Ok(Val::Unit);
                }
                self.at_expr(id, |l| l.b.ins_free_early(v, *deep).map_err(mir))?;
                Ok(Val::Unit)
            }
            // `region { … }`: its enter / leave HostInvoke.
            HirKind::Builtin { op: Builtin::Region { enter }, .. } => {
                let native = if *enter { common::GC_REGION_ENTER_ID } else { common::GC_REGION_EXIT_ID };
                let layout = common::HOST_ENUM_LAYOUT_BOXED as u8;
                self.at_expr(id, |l| l.b.ins_host_invoke_as(native, Vec::new(), layout, None).map_err(mir))?;
                Ok(Val::Unit)
            }
            HirKind::Builtin { op: Builtin::Done, args } if let [handle] = args.as_slice() => {
                let Some(h) = self.word(*handle)? else {
                    return Ok(Val::Never);
                };
                Ok(Val::V(self.b.ins_done_coro(h).map_err(mir)?))
            }
            // `panic msg`: the message and `Panic`, all at the panic's own
            // location, as HIR emission. A failed `requires` (a second
            // argument) blames the caller through `contract_fail`.
            HirKind::Builtin { op: Builtin::Panic, args } => {
                let (msg, blame) = match args.as_slice() {
                    [msg] => (*msg, false),
                    [msg, _] => (*msg, true),
                    _ => return Err("panic arguments".into()),
                };
                self.at_expr(id, |l| {
                    let Some(m) = l.word(msg)? else {
                        return Ok(Val::Never);
                    };
                    if l.b.value_ty(m) != MirTy::HeapRef {
                        return Err("panic message".into());
                    }
                    l.keep_pins()?;
                    // `contract_fail` reads the caller's frame, so it runs
                    // here in this body's own frame, as HIR emission's.
                    let m = if blame {
                        let native = super::host_allow::host_edge_spec_by_name(common::CONTRACT_FAIL_NATIVE).ok_or("panic blame native")?.id;
                        let layout = common::HOST_ENUM_LAYOUT_BOXED as u8;
                        l.b.ins_host_invoke_as(native, vec![m], layout, Some(MirTy::HeapRef)).map_err(mir)?
                    } else {
                        m
                    };
                    l.b.panic(m).map_err(mir)?;
                    Ok(Val::Never)
                })
            }
            // `readonly e`: `e`'s value.
            // A dictionary entry's argument or result (compiler-made
            // bodies): the same bits read as the node's type.
            HirKind::Builtin { op: Builtin::Retype, args } if let [inner] = args.as_slice() => {
                let ty = self.expr_ty(id).and_then(|t| self.word_or_object(t)).ok_or("retype type")?;
                // A two-word value read as a type parameter's word: its box.
                let v = if self.pair_tys(*inner).is_some() { self.boxed_word(*inner)? } else { self.word(*inner)? };
                let Some(v) = v else {
                    return Ok(Val::Never);
                };
                let word = self.b.ins_word(v).map_err(mir)?;
                Ok(Val::V(self.b.ins_word_as(word, ty).map_err(|_| "retype type")?))
            }
            // An `Option` / `Result` in another layout (compiler-made
            // bodies): a niche word boxed, or an enum object's niche word.
            HirKind::Builtin { op: Builtin::Relayout, args } if let [inner] = args.as_slice() => {
                let to = self.expr_ty(id).and_then(|t| self.word_or_object(t)).ok_or("relayout type")?;
                let Some(v) = self.word(*inner)? else {
                    return Ok(Val::Never);
                };
                // The enum object may be read as a plain word (`i64`).
                match (self.b.value_ty(v), to) {
                    (from, to) if from == to => Ok(Val::V(v)),
                    (from @ (MirTy::NicheOpt | MirTy::NicheRes), MirTy::HeapRef | MirTy::I64) => {
                        let layout = self.niche_layout(*inner, from)?;
                        let obj = self.box_niche(layout, v)?;
                        Ok(Val::V(self.fit(obj, to)?))
                    }
                    (MirTy::HeapRef | MirTy::I64, MirTy::NicheOpt | MirTy::NicheRes) => {
                        let obj = self.fit(v, MirTy::HeapRef)?;
                        self.unbox_niche(obj, to)
                    }
                    _ => Err("relayout".into()),
                }
            }
            // Copy-on-write share / unique: the operand's word, or a copy's.
            HirKind::Builtin { op: Builtin::Cow { unique }, args } if let [inner] = args.as_slice() => {
                // A two-word value (a type parameter's clone at a pair
                // type) is no object: it passes as it is.
                let v = match self.value(*inner)? {
                    Val::V(v) => v,
                    other => return Ok(other),
                };
                // A shared generic body's type parameter word may be a
                // scalar: the op runs where the body's `kinds` word says the
                // type is a copy-on-write object (#2506). A body with no
                // bit for it (a cleanup pad, an outlined helper) leaves it
                // out. Its per-type clones get the op at an object type.
                let open = self.expr_ty(*inner).is_some_and(|t| is_var(t) || is_var_app(t));
                if open
                    && matches!(self.b.value_ty(v), MirTy::I64 | MirTy::HeapRef)
                    && let Some(bit) = self.expr_ty(*inner).cloned().and_then(|t| self.kinds_bit(&t))
                {
                    return Ok(Val::V(self.guarded_cow(id, v, *unique, bit)?));
                }
                if self.b.value_ty(v) != MirTy::HeapRef || open {
                    return Ok(Val::V(v));
                }
                if let Some(Ty::Existential { class }) = self.expr_ty(*inner).map(strip_readonly) {
                    let class = class.clone();
                    return self.existential_cow(id, v, *unique, &class);
                }
                let out = self.at_expr(id, |l| l.b.ins_cow(v, *unique).map_err(mir))?;
                Ok(Val::V(self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![out]).map_err(mir)?))
            }
            // A primitive's text (compiler-made bodies).
            HirKind::Builtin { op: Builtin::Stringify { tag }, args } if let [inner] = args.as_slice() => {
                let Some(v) = self.word(*inner)? else {
                    return Ok(Val::Never);
                };
                let tag = *tag;
                let s = self.at_expr(id, |l| l.b.ins_stringify(v, tag).map_err(mir))?;
                Ok(Val::V(self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![s]).map_err(mir)?))
            }
            // `typeof e`: the type name HIR emission interned; `e` is not
            // evaluated.
            HirKind::Builtin { op: Builtin::TypeOf, .. } => {
                let &idx = self.site(id).string.as_ref().ok_or("typeof")?;
                Ok(Val::V(self.b.ins_string(idx).map_err(mir)?))
            }
            HirKind::Logic { and, lhs, rhs } => self.logic(*and, *lhs, *rhs),
            HirKind::Un { op, operand } => {
                let Some(v) = self.word(*operand)? else {
                    return Ok(Val::Never);
                };
                let out = match op {
                    UnOp::Neg if self.b.value_ty(v) == MirTy::I64 => self.at_expr(id, |l| Ok(l.b.ins_neg(v)))?,
                    UnOp::Neg => self.b.ins_neg(v),
                    UnOp::Not if self.b.value_ty(v) == MirTy::Bool => self.b.ins_not(v),
                    // `LogNot` of an int word: `!(x != 0)`.
                    UnOp::Not if self.b.value_ty(v) == MirTy::I64 && self.expr_ty(id).and_then(scalar) == Some(MirTy::Bool) => {
                        let zero = self.b.ins_const(MirConst::I64(0)).map_err(mir)?;
                        self.b.ins_cmp(MirCmpOp::Eq, v, zero)
                    }
                    UnOp::BitNot if self.b.value_ty(v) == MirTy::I64 => {
                        let ones = self.b.ins_const(MirConst::I64(-1)).map_err(mir)?;
                        self.b.ins_binop(MirBinOp::Xor, v, ones)
                    }
                    _ => return Err("unary operand".into()),
                };
                Ok(Val::V(out.map_err(mir)?))
            }
            HirKind::Cast { value } => {
                let names = (self.expr_ty(*value).and_then(classify::primitive), self.expr_ty(id).and_then(classify::primitive));
                // A cast between two heap words of no primitive type (a
                // byte string literal's array `as Vec<byte>` / `[byte; N]`)
                // emits no opcode: the value is its own word.
                if names == (None, None)
                    && let (Some(a), Some(b)) = (self.expr_ty(*value), self.expr_ty(id))
                    && self.word_ty(a) == Some(MirTy::HeapRef)
                    && self.word_ty(b) == Some(MirTy::HeapRef)
                {
                    return self.value(*value);
                }
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
                    // `CastIntToBool`: any non-zero int is `true`.
                    (Some(MirTy::I64), Some(MirTy::Bool)) => {
                        let zero = self.b.ins_const(MirConst::I64(0)).map_err(mir)?;
                        self.b.ins_cmp(MirCmpOp::Ne, v, zero).map_err(mir)?
                    }
                    // `CastBoolToInt`: a `bool` is its `0` / `1` word.
                    (Some(MirTy::Bool), Some(MirTy::I64)) => self.b.ins_cast(MirCastKind::Word, MirTy::I64, v).map_err(mir)?,
                    _ => return Err("cast".into()),
                };
                Ok(Val::V(out))
            }
            HirKind::Block { stmts, tail } => {
                // The `defer`s this block opens run at its end, after its
                // value (every path leaving it early ran them already).
                let open = self.defers.len();
                let val = self.block_body(id, stmts, *tail, returned);
                if self.defers.len() > open {
                    if matches!(val, Ok(Val::Never)) {
                        self.defers.truncate(open);
                    } else if val.is_ok() {
                        self.run_defers_from(open)?;
                        self.defers.truncate(open);
                    }
                }
                val
            }
            HirKind::Let { local, init: Some(init) } if self.stack(local.0).is_some() || self.sroa(local.0).is_some() => {
                self.fill_slots(*local, *init)
            }
            HirKind::Assign { place, value }
                if let HirKind::Local(local) = self.hir.expr(*place).kind
                    && self.stack(local.0).is_some() =>
            {
                self.fill_slots(local, *value)
            }
            // `x++` used for its value reads the old or new `x`.
            HirKind::Assign { place, value }
                if e.flags.contains(HirFlags::ADJUST) && self.adjust_store != Some(id) && !self.expr_ty(id).is_some_and(is_unit) =>
            {
                self.adjust_value(id, *place, *value)
            }
            HirKind::Assign { place, value } if let HirKind::Index { base, index, .. } = self.hir.expr(*place).kind => {
                self.stack_store(*place, base, index, *value)
            }
            HirKind::Assign { place, value } if let Some(slot) = self.frame_field(*place) => {
                if let Some(done) = self.cow_field(*place, *value, slot)? {
                    return Ok(done);
                }
                let boxed = self.boxed_slot_field(*place);
                let open = self.open_slot_niche(*place);
                let ty = self
                    .expr_ty(*place)
                    .and_then(|t| self.word_ty(t))
                    .filter(|_| open.is_none())
                    .or_else(|| (boxed || open.is_some()).then_some(MirTy::HeapRef))
                    .or_else(|| self.expr_ty(*place).is_some_and(is_heap_tuple).then_some(MirTy::HeapRef))
                    .ok_or("field type")?;
                // A two-word value is stored boxed, as the slot's `new` did;
                // so is an open niche field's.
                let v = match open {
                    Some(niche) => {
                        let layout = self.niche_layout(*place, niche)?;
                        self.boxed_niche_word(*value, layout)?
                    }
                    None if boxed => self.boxed_word(*value)?,
                    None => self.word(*value)?,
                };
                let Some(v) = v else {
                    return Ok(Val::Never);
                };
                if self.b.value_ty(v) != ty {
                    return Err("field type".into());
                }
                match self.boxed_field(*place, slot)? {
                    Some((obj, index)) => {
                        self.b.ins_heap_field_store(obj, v, index).map_err(mir)?;
                    }
                    None => {
                        // A field split into a frame slot is a local of its
                        // own: its rebind drops the old word, as a local's
                        // does (#1791).
                        let old = self.rebound_word(LocalId(slot), ty)?;
                        self.b.def_local(LocalId(slot), v).map_err(mir)?;
                        if let Some(old) = old {
                            self.b.ins_clear_word(old).map_err(mir)?;
                        }
                    }
                }
                Ok(Val::Unit)
            }
            // A `()` field: the record or object for its effects (#1510).
            HirKind::Field { base, .. } if self.expr_ty(id).is_some_and(is_unit) => {
                if !self.effect(*base)? {
                    return Ok(Val::Never);
                }
                Ok(Val::Unit)
            }
            HirKind::Field { base, .. }
                if self.site(id).field_new
                    && let Some((slot, ty)) = self.heap_field(id)
                    && self.news(*base).is_some()
                    && let HirKind::Make { args, .. } = &self.hir.expr(*base).kind =>
            {
                // As HIR emission's: the object is never observed, so its
                // words run in order and the field's is read back.
                let Some(words) = self.object_words(*base, &args.clone())? else {
                    return Ok(Val::Never);
                };
                let w = *words.get(slot as usize).ok_or("field slot")?;
                if self.b.value_ty(w) != ty {
                    return Err("field type".into());
                }
                match self.boxed_niche(id) {
                    Some(layout) => self.unbox_niche(w, niche_word(layout)?),
                    None => Ok(Val::V(w)),
                }
            }
            // A flat two-word field: its first slot and its tail slot.
            HirKind::Field { base, .. } if let Some((slot, tail, lo_ty, hi_ty)) = self.pair_field(id) => {
                let Some(obj) = self.word(*base)? else {
                    return Ok(Val::Never);
                };
                if self.b.value_ty(obj) != MirTy::HeapRef {
                    return Err("pair field base".into());
                }
                let lo = self.b.ins_product_load(obj, slot, lo_ty).map_err(mir)?;
                let hi = self.b.ins_product_load(obj, tail, hi_ty).map_err(mir)?;
                if self.field(id).invariant {
                    self.b.mark_invariant_load(lo);
                    self.b.mark_invariant_load(hi);
                }
                Ok(Val::Pair(lo, hi))
            }
            HirKind::Assign { place, value }
                if let HirKind::Field { base, .. } = self.hir.expr(*place).kind
                    && let Some((slot, tail, lo_ty, hi_ty)) = self.pair_field(*place) =>
            {
                // The value, then the object, as `SetField`: payload, then tag.
                let Some((lo, hi)) = self.pair(*value)? else {
                    return Ok(Val::Never);
                };
                if self.b.value_ty(lo) != lo_ty || self.b.value_ty(hi) != hi_ty {
                    return Err("pair field type".into());
                }
                let Some(obj) = self.word(base)? else {
                    return Ok(Val::Never);
                };
                self.b.ins_heap_field_store(obj, lo, slot).map_err(mir)?;
                self.b.ins_heap_field_store(obj, hi, tail).map_err(mir)?;
                Ok(Val::Unit)
            }
            // A field of a readonly class kept as its pair (#1562): that
            // word, with no object.
            HirKind::Field { base, .. }
                if let Some((slot, ty)) = self.heap_field(id)
                    && slot < 2
                    && self.pair_tys(*base).is_some()
                    && self.expr_ty(*base).is_some_and(|t| self.cx.pair_class(t).is_some()) =>
            {
                let w = match self.value(*base)? {
                    Val::Never => return Ok(Val::Never),
                    Val::Pair(lo, hi) => {
                        if slot == 0 {
                            lo
                        } else {
                            hi
                        }
                    }
                    // One word (a parameter, an element): its instance.
                    Val::V(obj) if self.b.value_ty(obj) == MirTy::HeapRef => {
                        let w = self.b.ins_product_load(obj, slot, ty).map_err(mir)?;
                        if self.field(id).invariant {
                            self.b.mark_invariant_load(w);
                        }
                        w
                    }
                    _ => return Err("field base".into()),
                };
                if self.b.value_ty(w) != ty {
                    return Err("field type".into());
                }
                Ok(Val::V(w))
            }
            HirKind::Field { base, .. } if let Some((slot, ty)) = self.heap_field(id) => {
                // A two-word enum is boxed for the read, as HIR emission's.
                let Some(obj) = self.boxed_word(*base)? else {
                    return Ok(Val::Never);
                };
                let w = self.b.ins_product_load(obj, slot, ty).map_err(mir)?;
                if self.field(id).invariant {
                    self.b.mark_invariant_load(w);
                }
                match self.boxed_niche(id) {
                    Some(layout) => self.unbox_niche(w, niche_word(layout)?),
                    None => Ok(Val::V(w)),
                }
            }
            HirKind::Assign { place, value }
                if let HirKind::Field { base, .. } = self.hir.expr(*place).kind
                    && let Some((slot, ty)) = self.heap_field(*place) =>
            {
                // `obj.f = unique(obj.f)`: one in-place op.
                if ty == MirTy::HeapRef && self.boxed_niche(*place).is_none() && self.same_field(*place, *value) {
                    let Some(obj) = self.word(base)? else {
                        return Ok(Val::Never);
                    };
                    let out = self.at_expr(*value, |l| l.b.ins_cow_at(obj, true, Some(slot)).map_err(mir))?;
                    self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![out]).map_err(mir)?;
                    return Ok(Val::Unit);
                }
                // The value, then the object, as HIR emission's `SetField`.
                let v = match self.boxed_niche(*place) {
                    Some(layout) => self.boxed_niche_word(*value, layout)?,
                    // A two-word value is stored boxed, as `new`'s.
                    None if ty == MirTy::HeapRef => self.boxed_word(*value)?,
                    None => self.word(*value)?,
                };
                let Some(v) = v else {
                    return Ok(Val::Never);
                };
                if self.b.value_ty(v) != ty {
                    return Err("field type".into());
                }
                let Some(obj) = self.word(base)? else {
                    return Ok(Val::Never);
                };
                self.b.ins_heap_field_store(obj, v, slot).map_err(mir)?;
                Ok(Val::Unit)
            }
            // A record's field: its slot.
            HirKind::Field { base, .. } if let Some((slot, ty)) = self.cx.record_field(id) => {
                let Some(obj) = self.word(*base)? else {
                    return Ok(Val::Never);
                };
                Ok(Val::V(self.b.ins_product_load(obj, slot, ty).map_err(mir)?))
            }
            HirKind::Assign { place, value }
                if let HirKind::Field { base, .. } = self.hir.expr(*place).kind
                    && let Some((slot, ty)) = self.cx.record_field(*place) =>
            {
                // The value, then the object, as `SetField`; a two-word
                // value boxed, a `()` the empty tuple object (#1510).
                let v = if self.expr_ty(*value).is_some_and(is_unit) {
                    self.word_or_boxed(*value)?
                } else if ty == MirTy::HeapRef {
                    self.boxed_word(*value)?
                } else {
                    self.word(*value)?
                };
                let Some(v) = v else {
                    return Ok(Val::Never);
                };
                if self.b.value_ty(v) != ty {
                    return Err("field type".into());
                }
                let Some(obj) = self.word(base)? else {
                    return Ok(Val::Never);
                };
                self.b.ins_heap_field_store(obj, v, slot).map_err(mir)?;
                Ok(Val::Unit)
            }
            HirKind::Make { kind: MakeKind::Record(_), args } if let Some((type_id, words)) = self.cx.record(id) => {
                // Each value in literal order, then the slotted object and
                // each value to its slot (the fields sorted by name).
                let mut slots = vec![None; words.len()];
                for (&arg, (slot, ty)) in args.iter().zip(words) {
                    // A `()` runs for its effect and is stored as the empty
                    // tuple object (#1510).
                    let v = if self.expr_ty(arg).is_some_and(is_unit) {
                        if !self.effect(arg)? {
                            return Ok(Val::Never);
                        }
                        self.unit_object()?
                    } else {
                        // A two-word value is stored boxed.
                        let Some(v) = (if ty == MirTy::HeapRef { self.boxed_word(arg)? } else { self.word(arg)? }) else {
                            return Ok(Val::Never);
                        };
                        if self.b.value_ty(v) != ty {
                            return Err("record field type".into());
                        }
                        v
                    };
                    *slots.get_mut(slot as usize).ok_or("record slot")? = Some(v);
                }
                let slots = slots.into_iter().collect::<Option<Vec<_>>>().ok_or("record slot")?;
                let nfields = slots.len() as u32;
                let obj = self.b.ins_alloc(MirAllocKind::Object { type_id, nfields }, Vec::new()).map_err(mir)?;
                let obj = self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![obj]).map_err(mir)?;
                for (slot, v) in slots.into_iter().enumerate() {
                    self.b.ins_heap_field_store(obj, v, slot as u32).map_err(mir)?;
                }
                Ok(Val::V(obj))
            }
            // An open niche field's box, read back as its niche word.
            HirKind::Field { .. } if let Some(slot) = self.frame_field(id) && let Some(niche) = self.open_slot_niche(id) => {
                let obj = match self.boxed_field(id, slot)? {
                    Some((obj, index)) => self.b.ins_product_load(obj, index, MirTy::HeapRef).map_err(mir)?,
                    None => self.b.use_local(LocalId(slot), MirTy::HeapRef).map_err(mir)?,
                };
                self.unbox_niche(obj, niche)
            }
            HirKind::Field { .. } if let Some(slot) = self.frame_field(id) => {
                let ty = self
                    .expr_ty(id)
                    .and_then(|t| self.word_ty(t))
                    .or_else(|| self.boxed_slot_field(id).then_some(MirTy::HeapRef))
                    // A heap tuple field's slot holds the tuple object (#1170).
                    .or_else(|| self.expr_ty(id).is_some_and(is_heap_tuple).then_some(MirTy::HeapRef))
                    .ok_or("field type")?;
                if let Some((obj, index)) = self.boxed_field(id, slot)? {
                    return Ok(Val::V(self.b.ins_product_load(obj, index, ty).map_err(mir)?));
                }
                Ok(Val::V(self.b.use_local(LocalId(slot), ty).map_err(mir)?))
            }
            HirKind::Index { base, index, .. }
                if let HirKind::Local(local) = self.hir.expr(*base).kind
                    && let Some((slot, n, ty)) = self.stack(local.0)
                    && !self.boxes.contains_key(&local.0) =>
            {
                let lit = matches!(self.hir.expr(*index).kind, HirKind::Lit(Lit::Int(i)) if (0..n as i64).contains(&i));
                if !lit && !query::stack_index_proven(self.hir, id, *index, n) {
                    let Some(idx) = self.index_word(*index)? else {
                        return Ok(Val::Never);
                    };
                    return Ok(Val::V(self.stack_select_checked(slot, n, idx, ty)?));
                }
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
                // A heap tuple (not kept as a pair): its two items read out.
                let heap = (self.pair_tys(*init).is_none())
                    .then(|| match self.expr_ty(*init).map(strip_readonly) {
                        Some(Ty::Tuple(tys)) if tys.len() == 2 => Some((self.tuple_item(&tys[0])?, self.tuple_item(&tys[1])?)),
                        _ => None,
                    })
                    .flatten();
                let ab = match heap {
                    Some((ta, tb)) => match self.word(*init)? {
                        Some(obj) if self.b.value_ty(obj) == MirTy::HeapRef => {
                            let a = self.b.ins_tuple_item(obj, 0, ta).map_err(mir)?;
                            Some((a, self.b.ins_tuple_item(obj, 1, tb).map_err(mir)?))
                        }
                        Some(_) => return Err("let pattern tuple".into()),
                        None => None,
                    },
                    None => self.pair(*init)?,
                };
                let Some((a, b)) = ab else {
                    return Ok(Val::Never);
                };
                let tys = match self.expr_ty(*init).map(strip_readonly) {
                    Some(Ty::Tuple(tys)) if tys.len() == 2 => Some(tys.clone()),
                    _ => None,
                };
                for (k, (item, v)) in items.iter().zip([a, b]).enumerate() {
                    self.bind_tuple_item(item, v, tys.as_ref().map(|t| &t[k]))?;
                }
                Ok(Val::Unit)
            }
            // `let (a, b, c) = e` of a heap tuple of any other length: its
            // items read out, as a nested tuple pattern's.
            HirKind::LetPat { pat: pat @ HirPat::Tuple(_), init } => {
                let Some(obj) = self.word(*init)? else {
                    return Ok(Val::Never);
                };
                let ty = self.expr_ty(*init).cloned();
                self.bind_tuple_item(pat, obj, ty.as_ref())?;
                Ok(Val::Unit)
            }
            // `let { a, b } = r` of a record: each bound field read from its
            // slot.
            HirKind::LetPat { pat: HirPat::Record(parts), init } if let Some(words) = self.cx.record_pat(id) => {
                let Some(obj) = self.word(*init)? else {
                    return Ok(Val::Never);
                };
                if self.b.value_ty(obj) != MirTy::HeapRef {
                    return Err("let pattern record".into());
                }
                for ((_, part), word) in parts.iter().zip(words) {
                    if let (HirPat::Bind(local), Some((slot, ty))) = (part, word) {
                        let v = self.b.ins_product_load(obj, slot, ty).map_err(mir)?;
                        self.bind(*local, v)?;
                    }
                }
                Ok(Val::Unit)
            }
            _ if query::is_unit_make(self.hir, id) => Ok(Val::Unit),
            HirKind::Make { kind, args } if self.pair_tys(id).is_some() => self.make_pair(id, kind, args),
            HirKind::Make { kind: MakeKind::Tuple, args } => self.make_tuple(args),
            HirKind::Make { kind: MakeKind::Range { .. }, args } if self.expr_ty(id).is_some_and(classify::is_open_range) => self.make_open_range(id, args),
            HirKind::Make { kind: MakeKind::Array, args } if self.heap_array(id) => match self.cx.frozen_array(id) {
                Some(slot) => self.frozen_array(id, args, slot),
                None => self.make_array(id, args),
            },
            HirKind::Make { kind: MakeKind::Class(_), args } if self.news(id).is_some() => self.make_object(id, args),
            HirKind::Make {
                kind: MakeKind::Variant { variant, .. },
                args,
            } if let Some(niche) = self.expr_ty(id).and_then(|t| self.cx.niche(t)) => {
                self.make_niche(variant, args, niche)
            }
            HirKind::Make {
                kind: MakeKind::Variant { enum_name, variant, tag: Some(tag), .. },
                args,
            } if let Some(tys) = self.expr_ty(id).and_then(|t| self.cx.boxed_enum(t, Some(variant))) => {
                self.make_boxed(enum_name, *tag, &tys, args)
            }
            // An `Option` / `Result` neither niche nor a pair (its payload
            // open in a generic class's shared method) is boxed too.
            HirKind::Make {
                kind: MakeKind::Variant { enum_name, variant, tag: Some(tag), .. },
                args,
            } if let Some(tys) = self.expr_ty(id).and_then(|t| self.cx.boxed_view(t, Some(variant))) => {
                self.make_boxed(enum_name, *tag, &tys, args)
            }
            // A scalar enum's variant is its backing constant.
            HirKind::Make {
                kind: MakeKind::Variant { enum_name, variant, .. },
                args,
            } if args.is_empty() && let Some(lit) = self.cx.scalar_variant(enum_name, variant) => {
                Ok(Val::V(self.scalar_lit(lit)?))
            }
            // `p[0]` / `p[1]` of a two-word tuple local. Any other index
            // reads the local's box, as a boxed tuple's field below.
            HirKind::Index { base, index, .. }
                if let HirKind::Local(local) = self.hir.expr(*base).kind
                    && let Some((lo, hi, lo_ty, hi_ty)) = self.pair_local(local)
                    && self.expr_ty(*base).is_some_and(|t| matches!(strip_readonly(t), Ty::Tuple(_)))
                    && let HirKind::Lit(Lit::Int(i @ (0 | 1))) = self.hir.expr(*index).kind =>
            {
                let (slot, ty) = if i == 0 { (lo, lo_ty) } else { (hi, hi_ty) };
                Ok(Val::V(self.b.use_local(slot, ty).map_err(mir)?))
            }
            // `e[0]` / `e[1]` of any other two-word tuple (a flat field, a
            // call): that word of its pair.
            HirKind::Index { base, index, .. }
                if self.pair_tys(*base).is_some()
                    && self.expr_ty(*base).is_some_and(|t| matches!(strip_readonly(t), Ty::Tuple(_)))
                    && let HirKind::Lit(Lit::Int(i @ (0 | 1))) = self.hir.expr(*index).kind =>
            {
                let Some((lo, hi)) = self.pair(*base)? else {
                    return Ok(Val::Never);
                };
                Ok(Val::V(if i == 0 { lo } else { hi }))
            }
            HirKind::Index { base, index, kind: IndexKind::Array } if self.heap_array(*base) => self.heap_index(id, *base, *index),
            // A boxed tuple's field: the same `Index`.
            HirKind::Index { base, index, kind: IndexKind::Tuple } => self.heap_index(id, *base, *index),
            HirKind::Index { base, index, kind: IndexKind::String } => self.string_index(id, *base, *index),
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
                kind: Some(ForInKind::Tuple { arity }),
            } => self.for_array(id, pat, *iterable, *body, None, Items::Tuple(*arity)),
            HirKind::ForIn {
                pat,
                iterable,
                body,
                kind: Some(ForInKind::Coroutine),
            } => self.for_coroutine(id, pat, *iterable, *body),
            HirKind::Match { scrutinee, arms } => {
                if self.stmt_matches.remove(&id) && !returned {
                    self.discard_arms.extend(arms.iter().map(|a| a.body));
                }
                self.match_(*scrutinee, arms, returned)
            }
            // `let _ = e`: nothing can read `_`; nor a `()` local's word.
            HirKind::Let { local, init: Some(init) } if self.hir.local(*local).name == "_" || self.unit_local(*local) => {
                Ok(if self.effect(*init)? { Val::Unit } else { Val::Never })
            }
            // A type-only `let x: T;` of a scalar: zero until assigned.
            HirKind::Let { local, init: None } => {
                let ty = self.local_ty(*local)?;
                let slot = self.slot(*local)?;
                let v = self.zero(ty)?;
                self.b.def_local(slot, v).map_err(mir)?;
                Ok(Val::Unit)
            }
            HirKind::Let { local, init } => {
                let init = init.ok_or("uninitialized let")?;
                let ty = self.local_ty(*local)?;
                let slot = self.slot(*local)?;
                let Some(v) = (if ty == MirTy::HeapRef { self.boxed_word_copied(init)? } else { self.word(init)? }) else {
                    return Ok(Val::Never);
                };
                if self.b.value_ty(v) != ty {
                    return Err("let type".into());
                }
                self.keep_pin(*local)?;
                self.b.def_local(slot, v).map_err(mir)?;
                Ok(Val::Unit)
            }
            // `x = e` of a two-slot local: both words rebound.
            HirKind::Assign { place, value }
                if let HirKind::Local(local) = self.hir.expr(*place).kind
                    && let Some((lo, hi, lo_ty, hi_ty)) = self.pair_local(local) =>
            {
                let Some((a, b)) = self.pair(*value)? else {
                    return Ok(Val::Never);
                };
                if self.b.value_ty(a) != lo_ty || self.b.value_ty(b) != hi_ty {
                    return Err("assign pair type".into());
                }
                self.b.def_local(lo, a).map_err(mir)?;
                self.b.def_local(hi, b).map_err(mir)?;
                Ok(Val::Unit)
            }
            // A `()` local holds no word: the value runs for its effect.
            HirKind::Assign { place, value }
                if let HirKind::Local(local) = self.hir.expr(*place).kind
                    && self.unit_local(local) =>
            {
                Ok(if self.effect(*value)? { Val::Unit } else { Val::Never })
            }
            HirKind::Assign { place, value } => {
                if let HirKind::Global { .. } = self.hir.expr(*place).kind {
                    return self.store_static(*place, *value);
                }
                let HirKind::Local(local) = self.hir.expr(*place).kind else {
                    return Err("assign place".into());
                };
                let ty = self.local_ty(local)?;
                let slot = self.slot(local)?;
                let Some(v) = (if ty == MirTy::HeapRef { self.boxed_word_copied(*value)? } else { self.word(*value)? }) else {
                    return Ok(Val::Never);
                };
                if self.b.value_ty(v) != ty {
                    return Err("assign type".into());
                }
                self.keep_pin(local)?;
                let old = self.rebound_word(slot, ty)?;
                self.b.def_local(slot, v).map_err(mir)?;
                if let Some(old) = old {
                    self.b.ins_clear_word(old).map_err(mir)?;
                }
                Ok(Val::Unit)
            }
            HirKind::If { cond, then, els } => self.if_(*cond, *then, *els, returned),
            // `while false { .. }` never runs: HIR emission drops it (and
            // plans nothing in it).
            HirKind::Loop { body }
                if query::while_shape(self.hir, *body).is_some_and(|(cond, _)| matches!(self.hir.expr(cond).kind, HirKind::Lit(Lit::Bool(false)))) =>
            {
                Ok(Val::Unit)
            }
            HirKind::Loop { body } => {
                let head = self.b.create_block();
                self.jump(head)?;
                self.switch(head);
                self.loops.push(Loop {
                    cont: head,
                    cont_used: false,
                    exit: None,
                    defers: self.defers.len(),
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
                let open = self.loops.last().ok_or("break outside loop")?.defers;
                self.run_defers_from(open)?;
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
            HirKind::Clear(locals) => {
                for &local in locals {
                    self.clear(local)?;
                }
                Ok(Val::Unit)
            }
            HirKind::Continue => {
                let open = self.loops.last().ok_or("continue outside loop")?.defers;
                self.run_defers_from(open)?;
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
            HirKind::ForIn {
                pat: HirPat::Bind(local),
                iterable,
                body,
                kind: Some(ForInKind::Custom { counted: None, .. }),
            } if let Some(plan) = self.site(id).iter_loop.as_ref() => self.for_iter(*local, *iterable, *body, plan),
            // A user `into_iter` returning a range or array runs its loop.
            HirKind::ForIn {
                pat: HirPat::Bind(local),
                iterable,
                body,
                kind: Some(ForInKind::Custom { counted: Some(counted), .. }),
            } if let Some(&target) = self.site(id).into_iter.as_ref() => match *counted {
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
                    Some(v) if self.ret_pair.is_none() && (query::is_unit_make(self.hir, *v) || self.expr_ty(*v).is_some_and(is_unit)) => {
                        if !self.effect(*v)? {
                            return Ok(Val::Never);
                        }
                        self.ret_missing(id)?;
                    }
                    Some(v) => {
                        self.ret_tail = true;
                        match self.ret_value(*v)? {
                            Val::Never => return Ok(Val::Never),
                            val => self.ret(val)?,
                        }
                    }
                    None if self.ret_pair.is_none() => self.ret_missing(id)?,
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
        let bounds = query::range_bounds(self.hir, iterable);
        let ty = self.local_ty(local)?;
        if ty != if float { MirTy::F64 } else { MirTy::I64 } {
            return Err("for in range type".into());
        }
        let x = self.slot(local)?;
        if !float && let Some((start, trips)) = query::unrolled_range(self.hir, iterable, body, inclusive) {
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
        // A body that assigns `x` steps a counter of its own, copied into
        // `x` each trip, as HIR emission's.
        let assigned = query::assigns_local(self.hir, body, local);
        // `a..b` written in place, or a range pair (a local or a call).
        let (start, end) = match bounds {
            _ if let Some(target) = into_iter => {
                let Some(obj) = self.word(iterable)? else {
                    return Ok(Val::Never);
                };
                let abi = CallAbi {
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
        let counter = if assigned { self.temp() } else { x };
        self.b.def_local(counter, start).map_err(mir)?;
        let head = self.b.create_block();
        let exit = self.b.create_block();
        let body_b = self.b.create_block();
        self.jump(head)?;
        self.switch(head);
        let cur = self.b.use_local(counter, ty).map_err(mir)?;
        let op = if inclusive { MirCmpOp::Le } else { MirCmpOp::Lt };
        let more = self.b.ins_cmp(op, cur, end).map_err(mir)?;
        self.b.branch(more, body_b, exit).map_err(mir)?;
        self.switch(body_b);
        if assigned {
            let cur = self.b.use_local(counter, ty).map_err(mir)?;
            self.b.def_local(x, cur).map_err(mir)?;
        }
        let one = if float { MirConst::F64(1.0_f64.to_bits()) } else { MirConst::I64(1) };
        // An inclusive `int` range leaves at its end before stepping past
        // it, which would overflow at `int` max (#1171).
        let last = (inclusive && !float).then_some(end);
        self.counted_body(body, head, exit, counter, ty, one, last)
    }

    /// A counted loop's body, then `counter += one` and back to `head`. The
    /// step ends the body's last block unless a `continue` needs it as its
    /// own target, as HIR emission places it (the shape MIR vectorize
    /// matches).
    /// `for x in h` over a coroutine, as HIR emission's: resume into `x`,
    /// stop once the handle is done (its completion value is never bound).
    fn for_coroutine(&mut self, id: HirId, pat: &HirPat, iterable: HirId, body: HirId) -> Result<Val, Refusal> {
        // A destructuring pattern reads its parts from each yielded tuple's
        // box (#2317).
        let (ty, item) = match pat {
            // A `()` comes as the empty tuple object, which nothing binds
            // (#2679).
            HirPat::Bind(local) if self.unit_local(*local) => (MirTy::HeapRef, None),
            HirPat::Bind(local) => (self.local_ty(*local)?, None),
            HirPat::Tuple(_) => {
                let item = match self.expr_ty(iterable).map(strip_readonly) {
                    Some(Ty::App(head, args)) if matches!(head.as_ref(), Ty::Con(n) if n == "coroutine") && args.len() == 2 => args[0].clone(),
                    _ => return Err("for in coroutine pattern".into()),
                };
                // A tuple (two-word ones too) comes back as its box.
                let heap = match self.word_ty(&item) {
                    Some(w) => w == MirTy::HeapRef,
                    None => matches!(strip_readonly(&item), Ty::Tuple(items) if !items.is_empty()),
                };
                if !heap {
                    return Err("for in coroutine pattern".into());
                }
                (MirTy::HeapRef, Some(item))
            }
            _ => return Err("for in coroutine pattern".into()),
        };
        let Some(h) = self.word(iterable)? else {
            return Ok(Val::Never);
        };
        let head = self.b.create_block();
        let body_b = self.b.create_block();
        let exit = self.b.create_block();
        self.jump(head)?;
        self.switch(head);
        let send = self.missing_send(id)?;
        let x = self.b.ins_resume_coro(h, send, ty).map_err(mir)?;
        let done = self.b.ins_done_coro(h).map_err(mir)?;
        self.b.branch(done, exit, body_b).map_err(mir)?;
        self.switch(body_b);
        // Bound once the generator is known to have yielded: the completing
        // resume's word is no value of the type (#2717).
        if let HirPat::Bind(local) = pat
            && !self.unit_local(*local)
        {
            self.bind(*local, x)?;
        }
        // The parts read only once the generator is known to have yielded.
        if let Some(item) = &item {
            self.bind_tuple_item(pat, x, Some(item))?;
        }
        // A loop over a generator it made (not one a local or field holds,
        // which a later `resume` may go on with) closes it when left
        // early: a `break` before it leaves (the loop's `defers` mark is
        // above the close), a `return` with the open defers. A `continue`
        // does not.
        let owned = !matches!(self.hir.expr(iterable).kind, HirKind::Local(_) | HirKind::Field { .. } | HirKind::Index { .. });
        let open = self.defers.len();
        if owned {
            self.defers.push(Open::Coro(h));
        }
        self.loops.push(Loop {
            cont: head,
            cont_used: false,
            exit: None,
            defers: self.defers.len(),
        });
        let live = self.effect(body);
        let lp = self.loops.pop().expect("loop pushed above");
        self.defers.truncate(open);
        if live? {
            self.jump(head)?;
        }
        if let Some(brk) = lp.exit {
            self.switch(brk);
            if owned {
                let native = self.cx.coro_close_native().ok_or("coro close native")?;
                self.b.ins_host_invoke(native, vec![h]).map_err(mir)?;
            }
            self.jump(exit)?;
        }
        self.switch(exit);
        Ok(Val::Unit)
    }

    /// `for x in it` over a user iterator, as HIR emission's: `into_iter`
    /// on the iterable once, then each trip `next` on the iterator, the
    /// loop left at `None` and `x` bound to the item.
    fn for_iter(&mut self, local: HirLocal, iterable: HirId, body: HirId, plan: &HirIterLoop) -> Result<Val, Refusal> {
        let ty = self.local_ty(local)?;
        let it_ty = self.word_ty(&plan.iter_ty).ok_or("iterator type")?;
        let Some(src) = self.word_or_boxed(iterable)? else {
            return Ok(Val::Never);
        };
        let it = match plan.into_iter {
            Some(into_iter) => {
                let mut words = vec![src];
                words.extend(self.dict_values(&plan.into_iter_dicts)?);
                let abi = CallAbi {
                    params: words.iter().map(|&v| self.b.value_ty(v)).collect(),
                    ret: it_ty,
                    ret_hi: None,
                };
                self.b.ins_call(into_iter, words, &abi).map_err(mir)?.0
            }
            None => src,
        };
        // `next`'s dictionaries, built once before the loop.
        let next_dicts = self.dict_values(&plan.next_dicts)?;
        let item = self.hir.local(local).ty.clone().ok_or("local type")?;
        let opt = crate::typechecking::ty::option_ty(item);
        let (ret, ret_hi) = match plan.shape {
            HirIterNext::Niche => {
                if self.word_ty(&opt) != Some(MirTy::NicheOpt) {
                    return Err("iterator next type".into());
                }
                (MirTy::NicheOpt, None)
            }
            HirIterNext::Pair => {
                let (_, variants) = self.cx.pair_variants(&opt).ok_or("iterator next type")?;
                let lo = variants.iter().find_map(|&(_, w)| w).ok_or("iterator next type")?;
                (lo, Some(MirTy::I64))
            }
            HirIterNext::Boxed { .. } => (MirTy::HeapRef, None),
        };
        let head = self.b.create_block();
        let body_b = self.b.create_block();
        let exit = self.b.create_block();
        self.jump(head)?;
        self.switch(head);
        let mut words = vec![it];
        words.extend(next_dicts.iter().copied());
        let abi = CallAbi {
            params: words.iter().map(|&v| self.b.value_ty(v)).collect(),
            ret,
            ret_hi,
        };
        let (w, hi) = self.b.ins_call(plan.next, words, &abi).map_err(mir)?;
        let x = match (plan.shape, hi) {
            (HirIterNext::Niche, _) => {
                let zero = self.b.ins_const(MirConst::I64(0)).map_err(mir)?;
                let none = self.b.ins_cmp(MirCmpOp::Eq, w, zero).map_err(mir)?;
                self.b.branch(none, exit, body_b).map_err(mir)?;
                self.switch(body_b);
                self.fit(w, ty)?
            }
            (HirIterNext::Pair, Some(tag)) => {
                let zero = self.b.ins_const(MirConst::I64(0)).map_err(mir)?;
                let none = self.b.ins_cmp(MirCmpOp::Eq, tag, zero).map_err(mir)?;
                self.b.branch(none, exit, body_b).map_err(mir)?;
                self.switch(body_b);
                self.fit(w, ty)?
            }
            (HirIterNext::Boxed { some }, _) => {
                let p = self.b.ins_match_payload(w, 0, ty).map_err(mir)?;
                self.b.jump_if_match(w, some, vec![p], body_b, exit).map_err(mir)?;
                self.switch(body_b);
                p
            }
            _ => return Err("iterator next result".into()),
        };
        self.bind(local, x)?;
        self.loops.push(Loop {
            cont: head,
            cont_used: false,
            exit: Some(exit),
            defers: self.defers.len(),
        });
        let live = self.effect(body);
        self.loops.pop();
        if live? {
            self.jump(head)?;
        }
        self.switch(exit);
        Ok(Val::Unit)
    }

    /// With `last`, the loop leaves once the counter reaches it, before the
    /// step.
    #[allow(clippy::too_many_arguments)]
    fn counted_body(&mut self, body: HirId, head: BlockId, exit: BlockId, counter: LocalId, ty: MirTy, one: MirConst, last: Option<ValueId>) -> Result<Val, Refusal> {
        let step = query::has_own_continue(self.hir, body).then(|| self.b.create_block());
        self.loops.push(Loop {
            cont: step.unwrap_or(head),
            cont_used: false,
            exit: Some(exit),
            defers: self.defers.len(),
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
        if let Some(last) = last {
            let done = self.b.ins_cmp(MirCmpOp::Eq, cur, last).map_err(mir)?;
            let step = self.b.create_block();
            self.b.branch(done, exit, step).map_err(mir)?;
            self.switch(step);
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
            BinOp::IntPow | BinOp::FloatPow => Some(MirBinOp::Pow),
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
        // `==` / `!=` on two-word values (an enum, tuple or range kept as
        // a pair): both boxed, then structural equality, as HIR
        // emission compares them.
        if matches!(op, BinOp::Eq | BinOp::Ne) && (self.pair_tys(lhs).is_some() || self.pair_tys(rhs).is_some()) {
            let Some(l) = self.boxed_word(lhs)? else {
                return Ok(Val::Never);
            };
            let Some(r) = self.boxed_word(rhs)? else {
                return Ok(Val::Never);
            };
            if self.b.value_ty(l) != MirTy::HeapRef || self.b.value_ty(r) != MirTy::HeapRef {
                return Err("operator operand type".into());
            }
            return self.structural_eq(l, r, op == BinOp::Ne);
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
        let strings = [lhs, rhs].iter().all(|&e| self.expr_ty(e).is_some_and(is_string));
        if strings && matches!(op, BinOp::Eq | BinOp::Ne) {
            return Ok(Val::V(self.b.ins_str_eq(l, r, op == BinOp::Ne).map_err(mir)?));
        }
        // `<` / `<=` / `>` / `>=` on strings: their byte order's sign
        // (`string_compare`) against `0` (coil-stdlib#48).
        if strings && let Some(cmp) = cmp {
            let order = self.b.ins_host_invoke(common::STRING_COMPARE_ID, vec![l, r]).map_err(mir)?;
            let zero = self.b.ins_const(MirConst::I64(0)).map_err(mir)?;
            return Ok(Val::V(self.b.ins_cmp(cmp, order, zero).map_err(mir)?));
        }
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
                if op.int_may_trap() || matches!(op, BinOp::Shl | BinOp::Shr) {
                    self.at_expr(id, |l2| Ok(l2.b.ins_binop(arith, l, r)))?
                } else {
                    self.b.ins_binop(arith, l, r)
                }
            }
            // `bool`s compare as their `0` / `1` words.
            (None, Some(cmp)) if lt == MirTy::Bool => {
                if self.b.value_ty(r) != MirTy::Bool {
                    return Err("operator operand type".into());
                }
                let l = self.b.ins_cast(MirCastKind::Word, MirTy::I64, l).map_err(mir)?;
                let r = self.b.ins_cast(MirCastKind::Word, MirTy::I64, r).map_err(mir)?;
                self.b.ins_cmp(cmp, l, r)
            }
            (None, Some(cmp)) => self.b.ins_cmp(cmp, l, r),
            (None, None) => unreachable!("refused above"),
        };
        Ok(Val::V(out.map_err(mir)?))
    }

    /// `==` / `!=` of two heap words: structural equality. An aggregate
    /// with a `float` leaf compares through its glue instead, so its
    /// floats compare by IEEE 754 (`codegen::eq_glue`, #1253).
    fn structural_eq(&mut self, l: ValueId, r: ValueId, ne: bool) -> Result<Val, Refusal> {
        Ok(Val::V(self.b.ins_str_eq(l, r, ne).map_err(mir)?))
    }

    /// Strength reduction: `x / 2^n` as `x >> n` when
    /// `x` is a `byte` or proved non-negative, and `x * 2^n` as `x << n`
    /// when `x` is a `byte`. An int multiply stays: a shift would not trap
    /// on overflow.
    fn strength_reduce(&self, op: BinOp, lhs: HirId, rhs: HirId) -> Option<(HirId, u32, MirBinOp)> {
        let pow2 = |id: HirId| self.int_imm(id).and_then(crate::const_fold::strength_div_int);
        let byte = |id: HirId| self.expr_ty(id).is_some_and(classify::is_byte);
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

    /// A bitwise identity or
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

    /// An integer literal, or a `const` local bound to one (`const`
    /// globals do not reach direct MIR).
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
        if query::logic_eager(self.hir, rhs) {
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
    /// source location (line breakpoints, source views). Nested statements keep their own.
    fn stmt(&mut self, id: HirId) -> Result<bool, Refusal> {
        self.at_stmt(id, |l| {
            l.box_before(id)?;
            l.effect(id)
        })
    }

    /// Box the stack arrays that escape first in statement (or block tail)
    /// `id` into one heap array each:
    /// every use after it in source order goes through the array.
    fn box_before(&mut self, id: HirId) -> Result<(), Refusal> {
        let Some(locals) = self.cx.box_at(id) else {
            return Ok(());
        };
        for &local in locals {
            // A frame-slot class local: an object of its class, each slot
            // stored to its field.
            if let (Some(tys), Some((type_id, nfields))) = (self.sroa(local), self.sroa_class(local)) {
                if tys.len() != nfields as usize {
                    return Err("boxed class local".into());
                }
                let tys = tys.clone();
                let base = self.slot(HirLocal(local))?.0;
                let mut words = Vec::with_capacity(tys.len());
                for (i, &ty) in tys.iter().enumerate() {
                    let slot = LocalId(base + i as u32);
                    if !self.b.local_defined(slot) {
                        return Err("boxed class local field".into());
                    }
                    words.push(self.b.use_local(slot, ty).map_err(mir)?);
                }
                let obj = self.b.ins_alloc(MirAllocKind::Object { type_id, nfields }, Vec::new()).map_err(mir)?;
                let obj = self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![obj]).map_err(mir)?;
                for (i, v) in words.into_iter().enumerate() {
                    self.b.ins_heap_field_store(obj, v, i as u32).map_err(mir)?;
                }
                let slot = self.temp();
                self.b.def_local(slot, obj).map_err(mir)?;
                self.boxes.insert(local, slot);
                continue;
            }
            let Some((base, n, ty)) = self.stack(local) else {
                return Err("boxed class local".into());
            };
            // Each slot's word as it is, as HIR emission's `MakeArray`: a
            // niche word stays the word, a `bool` its word.
            if !matches!(ty, MirTy::I64 | MirTy::F64 | MirTy::Bool | MirTy::HeapRef | MirTy::NicheOpt | MirTy::NicheRes) {
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

    /// For field `field` (slot `slot`) of a frame-slot class local boxed
    /// before here: the object and the field's index in it.
    /// `obj.f = unique(obj.f)` on a heap object's field (what copy-on-write
    /// puts before a write through `obj.f`): one in-place op that writes the
    /// field back only when it copied.
    fn cow_field(&mut self, place: HirId, value: HirId, slot: u32) -> Result<Option<Val>, Refusal> {
        if !self.same_field(place, value) || self.boxed_slot_field(place) || self.open_slot_niche(place).is_some() || self.expr_ty(place).and_then(|t| self.word_ty(t)) != Some(MirTy::HeapRef) {
            return Ok(None);
        }
        let Some((obj, index)) = self.boxed_field(place, slot)? else {
            return Ok(None);
        };
        let out = self.at_expr(value, |l| l.b.ins_cow_at(obj, true, Some(index)).map_err(mir))?;
        self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![out]).map_err(mir)?;
        Ok(Some(Val::Unit))
    }

    /// `value` is `unique(place)` and `place` is a field of a local: the
    /// assignment `place = value` copy-on-write puts before a write.
    fn same_field(&self, place: HirId, value: HirId) -> bool {
        let (HirKind::Field { base, name }, HirKind::Builtin { op: Builtin::Cow { unique: true }, args }) = (&self.hir.expr(place).kind, &self.hir.expr(value).kind) else {
            return false;
        };
        let ([inner], HirKind::Local(local)) = (args.as_slice(), &self.hir.expr(*base).kind) else {
            return false;
        };
        matches!(&self.hir.expr(*inner).kind, HirKind::Field { base: b, name: n } if n == name && matches!(self.hir.expr(*b).kind, HirKind::Local(l) if l == *local))
    }

    #[track_caller]
    fn boxed_field(&mut self, field: HirId, slot: u32) -> Result<Option<(ValueId, u32)>, Refusal> {
        let HirKind::Field { base, .. } = self.hir.expr(field).kind else {
            return Ok(None);
        };
        let HirKind::Local(local) = self.hir.expr(base).kind else {
            return Ok(None);
        };
        let Some(&boxed) = self.boxes.get(&local.0) else {
            return Ok(None);
        };
        let index = slot.checked_sub(self.slot(local)?.0).ok_or("boxed class field")?;
        Ok(Some((self.b.use_local(boxed, MirTy::HeapRef).map_err(mir)?, index)))
    }

    /// Whether field `id` of a frame-slot class local is a two-word enum
    /// its slot holds boxed (as HIR emission's `new` stored it).
    fn boxed_slot_field(&self, id: HirId) -> bool {
        let (Some(slot), HirKind::Field { base, .. }) = (self.frame_field(id), &self.hir.expr(id).kind) else {
            return false;
        };
        let HirKind::Local(local) = self.hir.expr(*base).kind else {
            return false;
        };
        let (Some(tys), Ok(at)) = (self.sroa(local.0), self.slot(local)) else {
            return false;
        };
        slot.checked_sub(at.0).and_then(|i| tys.get(i as usize)) == Some(&MirTy::HeapRef)
            && (self.pair_tys(id).is_some() || self.expr_ty(id).is_some_and(|t| self.word_ty(t).is_none() && self.cx.boxed_view(t, None).is_some()))
    }

    /// Whether field `id` of a frame-slot class local is a niche `Option`
    /// / `Result` its slot holds boxed (a generic class's field open in a
    /// parameter, as the class's heap object holds it): its niche word.
    fn open_slot_niche(&self, id: HirId) -> Option<MirTy> {
        let (Some(slot), HirKind::Field { base, .. }) = (self.frame_field(id), &self.hir.expr(id).kind) else {
            return None;
        };
        let HirKind::Local(local) = self.hir.expr(*base).kind else {
            return None;
        };
        let (Some(tys), Ok(at)) = (self.sroa(local.0), self.slot(local)) else {
            return None;
        };
        let niche = self.expr_ty(id).and_then(|t| self.word_ty(t)).filter(|w| matches!(w, MirTy::NicheOpt | MirTy::NicheRes))?;
        (slot.checked_sub(at.0).and_then(|i| tys.get(i as usize)) == Some(&MirTy::HeapRef)).then_some(niche)
    }

    /// The heap array a boxed stack array local `id` names.
    fn boxed(&self, id: HirId) -> Option<LocalId> {
        match self.hir.expr(id).kind {
            HirKind::Local(local) => self.boxes.get(&local.0).copied(),
            _ => None,
        }
    }

    /// The program's id of the body's inline scope `k` (`0`: none): the
    /// scope's call is located in the scope that call was in.
    fn program_scope(&mut self, file: u32, k: u32) -> u32 {
        let Some(i) = (k as usize).checked_sub(1) else { return 0 };
        if let Some(Some(id)) = self.scopes.get(i) {
            return *id;
        }
        let Some(s) = self.hir.inline_scopes.get(i) else { return 0 };
        let (callee, (start, end), call_scope, at_call) = (s.callee.clone(), s.call, s.call_scope, s.at_call);
        // Made before the scopes spliced into it: the chain ends.
        let call_scope = if call_scope < k { call_scope } else { 0 };
        let parent = self.program_scope(file, call_scope);
        // The call is in the code of the scope it was in.
        let file = self.scope_file(file, call_scope);
        let (start, end) = crate::variadic::written_span(start, end);
        let call = DebugLoc { file, start_byte: start as u32, end_byte: end.max(start + 1) as u32, scope: parent };
        let id = self.cx.inline_scope(common::InlineScope { callee, call, at_call });
        self.scopes[i] = Some(id);
        id
    }

    /// The file the code of the body's inline scope `k` is in: the body's
    /// (`file`), or another module's callee's (#2666).
    fn scope_file(&self, file: u32, k: u32) -> u32 {
        let mut k = k;
        while let Some(s) = (k as usize).checked_sub(1).and_then(|i| self.hir.inline_scopes.get(i)) {
            if let Some(f) = s.file {
                return f;
            }
            if s.call_scope >= k {
                break;
            }
            k = s.call_scope;
        }
        file
    }

    /// Where node `id` is in the source: its own span, or the one a
    /// foreign callee's spliced node keeps.
    fn src_span(&self, id: HirId) -> crate::hir::Span {
        let e = self.hir.expr(id);
        e.src.unwrap_or(e.span)
    }

    /// `f` with the source location of statement `id`.
    fn at_stmt<T>(&mut self, id: HirId, f: impl FnOnce(&mut Self) -> Result<T, Refusal>) -> Result<T, Refusal> {
        let outer = self.b.pending_loc;
        let (start, end) = self.src_span(id);
        // Generated code without a place of its own has the enclosing one,
        // but for a host call: the capability check finds it by its own span.
        // A compiler-made node ([`crate::hir::synth::SPAN`]) has none either.
        let placeless =
            (crate::attrs::is_fresh_span(start) && self.host_call(id).is_none()) || (start, end) == crate::hir::synth::SPAN;
        if let Some(file) = self.cx.file().filter(|_| !placeless) {
            let (start, end) = crate::variadic::written_span(start, end);
            let k = self.hir.expr(id).scope;
            let scope = self.program_scope(file, k);
            let file = self.scope_file(file, k);
            self.b.pending_loc = DebugLoc {
                file,
                start_byte: start as u32,
                end_byte: end.max(start + 1) as u32,
                scope,
            };
        }
        let out = f(self);
        self.b.pending_loc = outer;
        out
    }

    /// `f` with the source location of expression `id`: an op that can
    /// panic, or a call, reports its own line rather than its statement's
    /// (#841, #885).
    fn at_expr<T>(&mut self, id: HirId, f: impl FnOnce(&mut Self) -> Result<T, Refusal>) -> Result<T, Refusal> {
        self.at_stmt(id, f)
    }

    /// A plain call: `CALL` of the entry label HIR emission used, with the
    /// word ABI of the argument and result types (a pair as two words).
    fn call(&mut self, id: HirId, args: &[HirId]) -> Result<Val, Refusal> {
        if let Some(lane_call) = self.lane_call_of(id)
            && lane_call != HirLaneCall::Thunk
        {
            return self.lane_call(id, args, lane_call);
        }
        // A boxed two-word result kept as it comes back.
        let raw = std::mem::take(&mut self.raw_host);
        let Some(site) = self.site(id).call.as_ref() else {
            return Err(match &self.hir.expr(id).kind {
                HirKind::Call { callee: Callee::Named { name, .. } | Callee::Method { name }, .. } => format!("call `{name}`"),
                HirKind::Call { callee: Callee::Resolved(r), .. } => format!("call `{}`", r.key),
                _ => "call value".to_string(),
            });
        };
        if site.pair_args.len() != args.len() {
            return Err("call arity".into());
        }
        let ty = self.expr_ty(id).ok_or("call type")?;
        let unit = is_unit(ty);
        // Kept raw, a boxed niche result stays the enum object.
        let niche = if site.boxed_ret && !raw { Some(self.word_ty(ty).ok_or("call type")?) } else { None };
        let (ret, ret_hi) = match self.pair_tys(id) {
            _ if site.boxed_ret => (MirTy::HeapRef, None),
            Some((lo, hi)) if site.pair_ret => (lo, Some(hi)),
            // A callee returning the two-word value boxed (a depth-checked
            // body): the word, unboxed where a pair is read.
            Some(_) if raw || (!site.pair_ret && self.cx.boxed_view(ty, None).is_some()) => (MirTy::HeapRef, None),
            _ if site.pair_ret => return Err("call type".into()),
            _ if unit => (MirTy::I64, None),
            // A tuple not returned as a pair comes back boxed.
            _ if is_heap_tuple(ty) => (MirTy::HeapRef, None),
            // An enum value of no pair or niche layout: its boxed object.
            None if self.word_ty(ty).is_none() && self.cx.boxed_view(ty, None).is_some() => (MirTy::HeapRef, None),
            _ => (self.word_ty(ty).ok_or_else(|| format!("call type {}", ty_kind(ty)))?, None),
        };
        if site.word_ret && (site.pair_ret || site.coro || ret_hi.is_some()) {
            return Err("call unbox shape".into());
        }
        let mut words = Vec::with_capacity(args.len());
        for (i, (&arg, &pair)) in args.iter().zip(&site.pair_args).enumerate() {
            let word = site.words.get(i).copied().unwrap_or(false);
            // A two-word type not passed as a pair goes boxed.
            let boxed = !pair
                && self.expr_ty(arg).is_some_and(|t| {
                    // A numeric range's word is its typed object.
                    crate::layout::two_word_range_kind(t).is_some()
                        // A readonly class pair's word is its instance.
                        || self.cx.pair_class(t).is_some()
                        || (self.word_ty(t).is_none() && (self.cx.boxed_view(t, None).is_some() || is_heap_tuple(t)))
                });
            // A `()` goes as the empty tuple object, as HIR emission
            // passes it.
            if !pair && self.expr_ty(arg).is_some_and(|t| is_unit(t) && self.word_ty(t).is_none()) {
                if !self.effect(arg)? {
                    return Ok(Val::Never);
                }
                words.push(self.unit_object()?);
                continue;
            }
            if !pair && !boxed && self.expr_ty(arg).and_then(|t| self.word_ty(t)).is_none() {
                return Err("call argument type".into());
            }
            if word && pair {
                return Err("call argument word".into());
            }
            // A niche word passed in a boxed parameter: its enum object
            // (then boxed to a bare type parameter's tag).
            // A call returning its niche result as the boxed enum object
            // passes that object as it comes back (#2154).
            if !pair && let Some(&(_, layout)) = self.site(id).niche_args.iter().find(|&&(a, _)| a == arg.0) {
                self.raw_host = self.site(arg).call.as_ref().is_some_and(|s| s.boxed_ret);
                let w = self.word(arg);
                self.raw_host = false;
                let Some(w) = w? else {
                    return Ok(Val::Never);
                };
                let v = if self.b.value_ty(w) == MirTy::HeapRef { w } else { self.box_niche(layout, w)? };
                words.push(v);
                continue;
            }
            match (self.value(arg)?, pair) {
                (Val::Never, _) => return Ok(Val::Never),
                (Val::V(v), false) if boxed && self.b.value_ty(v) != MirTy::HeapRef => return Err("call argument boxed".into()),
                (Val::V(v), false) if let Some(&Some((target, operand))) = site.adapt.get(i) => {
                    if self.b.value_ty(v) != MirTy::HeapRef {
                        return Err("call argument adapter".into());
                    }
                    let f = self.at_expr(arg, |l| l.b.ins_alloc(MirAllocKind::Fn { target, operand, mask: 0 }, vec![v]).map_err(mir))?;
                    words.push(self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![f]).map_err(mir)?);
                }
                // A bare type parameter of a shared generic: the value's
                // own word (a `float` / `bool` as its bits).
                (Val::V(v), false) if word => {
                    words.push(self.b.ins_word(v).map_err(mir)?);
                }
                (Val::V(v), false) => words.push(v),
                (Val::Pair(lo, hi), false) if boxed => {
                    let ty = self.expr_ty(arg).ok_or("call argument type")?.clone();
                    let Val::V(v) = self.box_pair_val(&ty, lo, hi)? else {
                        return Ok(Val::Never);
                    };
                    words.push(v);
                }
                (Val::Pair(lo, hi), true) => words.extend([lo, hi]),
                // A boxed value the callee takes as a pair.
                (Val::V(v), true) if self.b.value_ty(v) == MirTy::HeapRef && let Some((lo_ty, hi_ty)) = self.pair_tys(arg) => {
                    let ty = self.expr_ty(arg).ok_or("call argument type")?.clone();
                    match self.unbox_word(&ty, v, lo_ty, hi_ty)? {
                        Val::Pair(lo, hi) => words.extend([lo, hi]),
                        _ => return Ok(Val::Never),
                    }
                }
                _ => return Err("call argument".into()),
            }
        }
        // The instance dictionaries, after the arguments.
        if !site.dicts.is_empty() {
            let dicts = site.dicts.clone();
            words.extend(self.dict_values(&dicts)?);
        }
        // `MakeCoro`: the arguments move into a new coroutine's frame.
        if site.coro {
            if ret != MirTy::HeapRef || ret_hi.is_some() {
                return Err("coroutine handle type".into());
            }
            let h = self.b.ins_alloc(MirAllocKind::Coro { target: site.target }, words).map_err(mir)?;
            return Ok(Val::V(self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![h]).map_err(mir)?));
        }
        let abi = CallAbi {
            params: words.iter().map(|&v| self.b.value_ty(v)).collect(),
            ret: if site.word_ret { MirTy::HeapRef } else { ret },
            ret_hi,
        };
        let (dest, dest_hi) = self.at_expr(id, |l| l.b.ins_call(site.target, words, &abi).map_err(mir))?;
        // A planned tail call: its callee may take this frame when the
        // call's result is what the body returns.
        if self.cx.tail_call(id) && !site.word_ret && niche.is_none() && site.ret_items.is_none() {
            self.b.allow_tail_call(site.target);
        }
        if let HirKind::Call { callee: Callee::Named { name, .. }, .. } = &self.hir.expr(id).kind {
            // A pure function's one scalar word: an operator that may trap.
            if self.cx.pure_fn(name)
                && !site.word_ret
                && site.dicts.iter().all(|d| matches!(d, HirDictOp::Kinds(_) | HirDictOp::KindsBit { .. }))
                && niche.is_none()
                && dest_hi.is_none()
                && ret.is_numeric()
            {
                self.b.mark_pure_call(dest);
            }
            if self.cx.steady_fn(name) {
                self.b.mark_steady_call(dest);
            }
        }
        // A shared generic body's object holding its type parameters'
        // values boxed, rebuilt with them unboxed (#1549).
        let dest = match &site.ret_items {
            Some(items) if !site.word_ret && site.ret_adapt.is_none() && dest_hi.is_none() && self.b.value_ty(dest) == MirTy::HeapRef => {
                match self.at_expr(id, |l| l.unbox_ret_items(dest, items))? {
                    Val::V(v) => v,
                    _ => return Err("call result items".into()),
                }
            }
            Some(_) => return Err("call result items".into()),
            None => dest,
        };
        // A shared generic body's closure, adapted to the call's ground
        // type (#1532).
        if let Some((target, operand)) = site.ret_adapt {
            if site.word_ret || niche.is_some() || dest_hi.is_some() || ret != MirTy::HeapRef {
                return Err("call result adapter".into());
            }
            let f = self.at_expr(id, |l| l.b.ins_alloc(MirAllocKind::Fn { target, operand, mask: 0 }, vec![dest]).map_err(mir))?;
            return Ok(Val::V(self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![f]).map_err(mir)?));
        }
        // A bare type parameter result comes back as its word; a niche
        // one as its enum object (#1565).
        if site.word_ret {
            if let Some(want) = niche {
                let obj = self.b.ins_word_as(dest, MirTy::HeapRef).map_err(mir)?;
                return self.unbox_niche(obj, want);
            }
            // `T = ()`: the word is the empty tuple object, and a `()` is
            // nothing to read (#2489).
            if unit {
                return Ok(Val::Unit);
            }
            return Ok(Val::V(self.b.ins_word_as(dest, ret).map_err(mir)?));
        }
        if let Some(want) = niche {
            return self.unbox_niche(dest, want);
        }
        if dest_hi.is_some() && self.heap_pair(id) {
            self.b.mark_heap_pair_word(dest);
        }
        Ok(match dest_hi {
            Some(hi) => Val::Pair(dest, hi),
            None if unit => Val::Unit,
            None => Val::V(dest),
        })
    }

    /// Shared generic body result `obj` rebuilt with the items `items`
    /// marks unboxed ([`HirRetItems`], #1549).
    fn unbox_ret_items(&mut self, obj: ValueId, items: &HirRetItems) -> Result<Val, Refusal> {
        let item = |l: &mut Self, it: &HirRetItem, v: ValueId| -> Result<ValueId, Refusal> {
            if it.unbox { l.b.ins_word_as(v, it.ty).map_err(mir) } else { Ok(v) }
        };
        let read_ty = |it: &HirRetItem| if it.unbox { MirTy::HeapRef } else { it.ty };
        match items {
            HirRetItems::Tuple { items, kinds } => {
                let mut words = Vec::with_capacity(items.len());
                for (i, it) in items.iter().enumerate() {
                    let v = self.b.ins_tuple_item(obj, i as u32, read_ty(it)).map_err(mir)?;
                    words.push(item(self, it, v)?);
                }
                let t = self.b.ins_alloc_kinds(MirAllocKind::TUPLE, words, *kinds).map_err(mir)?;
                Ok(Val::V(self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![t]).map_err(mir)?))
            }
            HirRetItems::Enum { variants } => {
                let mut arms = Vec::with_capacity(variants.len() + 1);
                for (tag, items, kinds) in variants {
                    let payload = items
                        .iter()
                        .enumerate()
                        .map(|(i, it)| self.b.ins_match_payload(obj, i as u32, read_ty(it)))
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(mir)?;
                    let (hit, miss) = (self.b.create_block(), self.b.create_block());
                    self.b.jump_if_match(obj, *tag, payload.clone(), hit, miss).map_err(mir)?;
                    self.switch(hit);
                    let mut words = Vec::with_capacity(items.len());
                    for (it, &v) in items.iter().zip(&payload) {
                        words.push(item(self, it, v)?);
                    }
                    let e = self.b.ins_alloc_kinds(MirAllocKind::Enum { tag: *tag }, words, *kinds).map_err(mir)?;
                    let e = self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![e]).map_err(mir)?;
                    arms.push((self.cur, Val::V(e)));
                    self.switch(miss);
                }
                arms.push((self.cur, Val::V(obj)));
                self.join_arms(arms)
            }
        }
    }

    /// The words of instance dictionary program `ops`: each method's
    /// `CodePtr`, each dictionary a tuple of the words below it.
    fn dict_values(&mut self, ops: &[HirDictOp]) -> Result<Vec<ValueId>, Refusal> {
        self.dict_program(Vec::new(), ops)
    }

    /// Run dictionary program `ops` on `stack`, giving the words left.
    fn dict_program(&mut self, mut stack: Vec<ValueId>, ops: &[HirDictOp]) -> Result<Vec<ValueId>, Refusal> {
        for op in ops {
            match *op {
                HirDictOp::Code(target) => stack.push(self.b.ins_code_ptr(target).map_err(mir)?),
                HirDictOp::Null => stack.push(self.b.ins_const(MirConst::I64(0)).map_err(mir)?),
                HirDictOp::Tuple(n) => {
                    let at = stack.len().checked_sub(n as usize).ok_or("dictionary arity")?;
                    let elems = stack.split_off(at);
                    let dict = self.b.ins_alloc(MirAllocKind::TUPLE, elems).map_err(mir)?;
                    stack.push(self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![dict]).map_err(mir)?);
                }
                HirDictOp::Load(slot) => stack.push(*self.dict_params.get(&slot).ok_or("dictionary parameter")?),
                HirDictOp::Word => {
                    let v = stack.pop().ok_or("dictionary word")?;
                    stack.push(self.b.ins_word(v).map_err(mir)?);
                }
                HirDictOp::Interned { slot, ref ops } => stack.push(self.interned_dict(slot, ops)?),
                HirDictOp::Kinds(bits) => stack.push(self.b.ins_const(MirConst::I64(bits as i64)).map_err(mir)?),
                HirDictOp::KindsBit { from, to } => {
                    let w = stack.pop().ok_or("kinds word")?;
                    // A caller with no `kinds` word of its own (a defer
                    // pad) passes the bit clear: no op, as before.
                    let Some(own) = self.kinds else {
                        stack.push(w);
                        continue;
                    };
                    let mut bit = own;
                    if from != to {
                        let k = self.b.ins_const(MirConst::I64(i64::from(from))).map_err(mir)?;
                        bit = self.b.ins_binop(MirBinOp::Shr, bit, k).map_err(mir)?;
                        let one = self.b.ins_const(MirConst::I64(1)).map_err(mir)?;
                        bit = self.b.ins_binop(MirBinOp::BitAnd, bit, one).map_err(mir)?;
                        if to > 0 {
                            let k = self.b.ins_const(MirConst::I64(i64::from(to))).map_err(mir)?;
                            bit = self.b.ins_binop(MirBinOp::Shl, bit, k).map_err(mir)?;
                        }
                    } else {
                        let m = self.b.ins_const(MirConst::I64((1u64 << to) as i64)).map_err(mir)?;
                        bit = self.b.ins_binop(MirBinOp::BitAnd, bit, m).map_err(mir)?;
                    }
                    stack.push(self.b.ins_binop(MirBinOp::BitOr, w, bit).map_err(mir)?);
                }
                HirDictOp::KindsOwn => stack.push(self.kinds.ok_or("kinds word")?),
            }
        }
        Ok(stack)
    }

    /// The entry check of a `kinds` word for `n` type parameters (#2506):
    /// its bits past the mask are the sentinel's, else the entry was
    /// called without it (a compiler bug), and panics with message `msg`.
    fn check_kinds(&mut self, kinds: ValueId, n: u32, msg: u32) -> Result<(), Refusal> {
        let high = if n >= 64 { 0 } else { u64::MAX << n };
        if high == 0 {
            return Ok(());
        }
        let m = self.b.ins_const(MirConst::I64(high as i64)).map_err(mir)?;
        let got = self.b.ins_binop(MirBinOp::BitAnd, kinds, m).map_err(mir)?;
        let want = self.b.ins_const(MirConst::I64((crate::codegen::kinds_sentinel(n)) as i64)).map_err(mir)?;
        let bad = self.b.ins_cmp(MirCmpOp::Ne, got, want).map_err(mir)?;
        let (fail, ok) = (self.b.create_block(), self.b.create_block());
        self.b.branch(bad, fail, ok).map_err(mir)?;
        self.switch(fail);
        let text = self.b.ins_string(msg).map_err(mir)?;
        self.b.panic(text).map_err(mir)?;
        self.switch(ok);
        Ok(())
    }

    /// One `kinds` word of `n` bits from dictionary word `d` (its first
    /// `dn` bits) and entry word `p` (the rest).
    fn merge_kinds(&mut self, d: ValueId, dn: u32, p: ValueId, n: u32) -> Result<ValueId, Refusal> {
        let low = |k: u32| if k >= 64 { u64::MAX } else { (1u64 << k) - 1 };
        let dm = self.b.ins_const(MirConst::I64(low(dn) as i64)).map_err(mir)?;
        let d = self.b.ins_binop(MirBinOp::BitAnd, d, dm).map_err(mir)?;
        let pm = self.b.ins_const(MirConst::I64(low(n - dn) as i64)).map_err(mir)?;
        let mut p = self.b.ins_binop(MirBinOp::BitAnd, p, pm).map_err(mir)?;
        if dn > 0 {
            let k = self.b.ins_const(MirConst::I64(i64::from(dn))).map_err(mir)?;
            p = self.b.ins_binop(MirBinOp::Shl, p, k).map_err(mir)?;
        }
        let w = self.b.ins_binop(MirBinOp::BitOr, d, p).map_err(mir)?;
        let s = self.b.ins_const(MirConst::I64(crate::codegen::kinds_sentinel(n) as i64)).map_err(mir)?;
        self.b.ins_binop(MirBinOp::BitOr, w, s).map_err(mir)
    }

    /// The bit of the body's `kinds` word that says whether a value of
    /// type `ty` (a type parameter, or one applied) is a copy-on-write
    /// object, with the word: `None` where the body has none for it.
    fn kinds_bit(&self, ty: &Ty) -> Option<(ValueId, u32)> {
        let kinds = self.kinds?;
        let var = match strip_readonly(ty) {
            Ty::Var(v) => *v,
            Ty::App(head, _) => match strip_readonly(head) {
                Ty::Var(v) => *v,
                _ => return None,
            },
            _ => return None,
        };
        let bit = self.cx.frame().kinds.as_ref()?.bit(var)?;
        Some((kinds, bit))
    }

    /// Copy-on-write op `unique` on word `v` of a type parameter whose
    /// `kinds` bit is `bit` of `kinds`: the op where the bit is set (the
    /// word is an object), the word as it is where it is clear.
    fn guarded_cow(&mut self, id: HirId, v: ValueId, unique: bool, (kinds, bit): (ValueId, u32)) -> Result<ValueId, Refusal> {
        let ty = self.b.value_ty(v);
        let m = self.b.ins_const(MirConst::I64((1u64 << bit) as i64)).map_err(mir)?;
        let set = self.b.ins_binop(MirBinOp::BitAnd, kinds, m).map_err(mir)?;
        let zero = self.b.ins_const(MirConst::I64(0)).map_err(mir)?;
        let object = self.b.ins_cmp(MirCmpOp::Ne, set, zero).map_err(mir)?;
        let (op, skip, join) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
        self.b.branch(object, op, skip).map_err(mir)?;
        self.switch(op);
        let obj = self.b.ins_word_as(v, MirTy::HeapRef).map_err(mir)?;
        let out = self.at_expr(id, |l| l.b.ins_cow(obj, unique).map_err(mir))?;
        // A share sets a flag on the object and gives it back: the word is
        // the same, so nothing joins.
        if !unique {
            self.jump(join)?;
            self.switch(skip);
            self.jump(join)?;
            self.switch(join);
            return Ok(v);
        }
        let out = self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![out]).map_err(mir)?;
        let out = self.b.ins_word_as(out, ty).map_err(mir)?;
        let op_from = self.cur;
        self.jump(join)?;
        self.switch(skip);
        self.jump(join)?;
        self.switch(join);
        self.b.ins_stack_phi(vec![(op_from, out), (skip, v)]).map_err(mir)
    }

    /// Ground dictionary `ops` held in static `slot`: built and stored on
    /// the first run in this runtime (a worker's statics start zeroed), loaded
    /// after that.
    fn interned_dict(&mut self, slot: u32, ops: &[HirDictOp]) -> Result<ValueId, Refusal> {
        let have = self.b.ins_load_static(slot, MirTy::HeapRef).map_err(mir)?;
        let zero = self.b.ins_const(MirConst::I64(0)).map_err(mir)?;
        let unset = self.b.ins_cmp(MirCmpOp::Eq, have, zero).map_err(mir)?;
        let build = self.b.create_block();
        let loaded = self.b.create_block();
        let join = self.b.create_block();
        self.b.branch(unset, build, loaded).map_err(mir)?;
        self.switch(build);
        let [built] = self.dict_program(Vec::new(), ops)?[..] else {
            return Err("interned dictionary arity".into());
        };
        self.b.ins_store_static(slot, built).map_err(mir)?;
        let built_from = self.cur;
        self.jump(join)?;
        self.switch(loaded);
        self.jump(join)?;
        self.switch(join);
        self.b.ins_stack_phi(vec![(built_from, built), (loaded, have)]).map_err(mir)
    }

    /// An auto-par call as HIR emission left it: the fork-join worker on
    /// the `int` arguments and the `hops` budget, or, for a one-argument
    /// site with a cutoff, `arg <= cutoff - 1 ? seq(arg) : worker(arg, hops)`.
    fn par_fork(&mut self, id: HirId, args: &[HirId], fork: &HirParFork) -> Result<Val, Refusal> {
        let ty = self.expr_ty(id).ok_or("call type")?;
        let ret = self.word_ty(ty).ok_or_else(|| format!("call type {}", ty_kind(ty)))?;
        let mut words = Vec::with_capacity(args.len() + 1);
        for &arg in args {
            let Some(v) = self.word(arg)? else {
                return Ok(Val::Never);
            };
            if self.b.value_ty(v) != MirTy::I64 {
                return Err("parallel call word".into());
            }
            words.push(v);
        }
        let worker = |l: &mut Self, mut words: Vec<ValueId>| -> Result<ValueId, Refusal> {
            words.push(l.b.ins_const(MirConst::I64(fork.hops)).map_err(mir)?);
            let abi = CallAbi {
                params: vec![MirTy::I64; words.len()],
                ret,
                ret_hi: None,
            };
            Ok(l.at_expr(id, |l| l.b.ins_call(fork.worker, words, &abi).map_err(mir))?.0)
        };
        let Some((seq, cutoff)) = fork.seq else {
            return Ok(Val::V(worker(self, words)?));
        };
        let [arg] = words[..] else {
            return Err("parallel call arity".into());
        };
        let limit = self.b.ins_const(MirConst::I64(cutoff - 1)).map_err(mir)?;
        let small = self.b.ins_cmp(MirCmpOp::Le, arg, limit).map_err(mir)?;
        let seq_block = self.b.create_block();
        let par_block = self.b.create_block();
        let join = self.b.create_block();
        self.b.branch(small, seq_block, par_block).map_err(mir)?;
        self.switch(seq_block);
        let abi = CallAbi {
            params: vec![MirTy::I64],
            ret,
            ret_hi: None,
        };
        let (low, _) = self.at_expr(id, |l| l.b.ins_call(seq, vec![arg], &abi).map_err(mir))?;
        let low_from = self.cur;
        self.jump(join)?;
        self.switch(par_block);
        let high = worker(self, vec![arg])?;
        let high_from = self.cur;
        self.jump(join)?;
        self.switch(join);
        Ok(Val::V(self.b.ins_stack_phi(vec![(low_from, low), (high_from, high)]).map_err(mir)?))
    }

    /// A parallel loop as HIR emission left it: its fork-join function
    /// called on the accumulator, the live captures and the bounds, the
    /// result the new accumulator.
    fn par_loop(&mut self, id: HirId, par: &HirParLoop) -> Result<Val, Refusal> {
        let mut words = Vec::with_capacity(par.args.len());
        for &local in &par.args {
            if self.local_ty(local)? != MirTy::I64 {
                return Err("parallel loop word".into());
            }
            let slot = self.slot(local)?;
            words.push(self.b.use_local(slot, MirTy::I64).map_err(mir)?);
        }
        let abi = CallAbi {
            params: vec![MirTy::I64; words.len()],
            ret: MirTy::I64,
            ret_hi: None,
        };
        let (dest, _) = self.at_expr(id, |l| l.b.ins_call(par.target, words, &abi).map_err(mir))?;
        if self.local_ty(par.acc)? != MirTy::I64 {
            return Err("parallel loop word".into());
        }
        let acc = self.slot(par.acc)?;
        self.b.def_local(acc, dest).map_err(mir)?;
        let Some((index, after)) = par.index else {
            return Ok(Val::Unit);
        };
        if self.local_ty(index)? != MirTy::I64 {
            return Err("parallel loop word".into());
        }
        let slot = self.slot(index)?;
        match after {
            HirParIndex::Const(n) => {
                let v = self.b.ins_const(MirConst::I64(n)).map_err(mir)?;
                self.b.def_local(slot, v).map_err(mir)?;
            }
            // `i = begin; if i < end { i = end }`, as HIR emission; a
            // longer stride lands on `begin + trips * stride`. The loop
            // ran, so its last step computed that value: nothing here
            // overflows.
            HirParIndex::Past { begin, end, stride } => {
                let b = self.par_bound(begin)?;
                let e = self.par_bound(end)?;
                self.b.def_local(slot, b).map_err(mir)?;
                let less = self.b.ins_cmp(MirCmpOp::Lt, b, e).map_err(mir)?;
                let raise = self.b.create_block();
                let join = self.b.create_block();
                self.b.branch(less, raise, join).map_err(mir)?;
                self.switch(raise);
                let past = if stride == 1 {
                    e
                } else {
                    let span = self.b.ins_binop(MirBinOp::Sub, e, b).map_err(mir)?;
                    let round = self.b.ins_const(MirConst::I64(stride - 1)).map_err(mir)?;
                    let span = self.b.ins_binop(MirBinOp::Add, span, round).map_err(mir)?;
                    let step = self.b.ins_const(MirConst::I64(stride)).map_err(mir)?;
                    let trips = self.b.ins_binop(MirBinOp::Div, span, step).map_err(mir)?;
                    let run = self.b.ins_binop(MirBinOp::Mul, trips, step).map_err(mir)?;
                    self.b.ins_binop(MirBinOp::Add, b, run).map_err(mir)?
                };
                self.b.def_local(slot, past).map_err(mir)?;
                self.jump(join)?;
                self.switch(join);
            }
        }
        Ok(Val::Unit)
    }

    /// A dynamic parallel loop's bound, as an `int` word.
    fn par_bound(&mut self, bound: HirParBound) -> Result<ValueId, Refusal> {
        let (local, bias) = match bound {
            HirParBound::Const(n) => return self.b.ins_const(MirConst::I64(n)).map_err(mir),
            HirParBound::Local(local, bias) => (local, bias),
        };
        if self.local_ty(local)? != MirTy::I64 {
            return Err("parallel loop word".into());
        }
        let slot = self.slot(local)?;
        let v = self.b.use_local(slot, MirTy::I64).map_err(mir)?;
        if bias == 0 {
            return Ok(v);
        }
        let bias = self.b.ins_const(MirConst::I64(bias)).map_err(mir)?;
        self.b.ins_binop(MirBinOp::Add, v, bias).map_err(mir)
    }

    /// A function value, as HIR emission builds it: each capture's word,
    /// then `MakeFn` of the code (or `MakePolyFn`).
    fn fn_value(&mut self, id: HirId, f: &HirFnValue) -> Result<Val, Refusal> {
        if self.expr_ty(id).and_then(|t| self.word_ty(t)) != Some(MirTy::HeapRef) {
            return Err("function value type".into());
        }
        let h = self.alloc_fn_value(f)?;
        Ok(Val::V(self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![h]).map_err(mir)?))
    }

    /// The allocation of function value `f`: its captures' words, then its
    /// dictionaries.
    fn alloc_fn_value(&mut self, f: &HirFnValue) -> Result<ValueId, Refusal> {
        // The captures made here first, before any other word is read.
        let mut remade = HashMap::new();
        for (at, value) in &f.remade {
            let h = self.alloc_fn_value(value)?;
            remade.insert(*at, self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![h]).map_err(mir)?);
        }
        let mut words = Vec::with_capacity(f.captures.len());
        for (i, &local) in f.captures.iter().enumerate() {
            if let Some(&h) = remade.get(&i) {
                words.push(h);
                continue;
            }
            if f.pairs.get(i).copied().unwrap_or(false) {
                let (lo, hi, lo_ty, hi_ty) = self.pair_local(local).ok_or("pair capture")?;
                words.push(self.b.use_local(lo, lo_ty).map_err(mir)?);
                words.push(self.b.use_local(hi, hi_ty).map_err(mir)?);
                continue;
            }
            words.push(self.local_word(local)?);
        }
        words.extend(self.dict_values(&f.dicts)?);
        self.b.ins_alloc(f.kind, words).map_err(mir)
    }

    /// A partial application `f(args)`, as HIR emission builds it: each
    /// filled argument's word, then `MakeFn` with its fill mask.
    fn partial(&mut self, id: HirId, f: &HirFnValue, args: &[HirId]) -> Result<Val, Refusal> {
        if self.expr_ty(id).and_then(|t| self.word_ty(t)) != Some(MirTy::HeapRef) {
            return Err("function value type".into());
        }
        let mut words = Vec::with_capacity(args.len());
        for &arg in args {
            // A two-word immediate product is boxed, as the parameter
            // takes it (#2790).
            let Some(v) = self.word_or_boxed(arg)? else {
                return Ok(Val::Never);
            };
            words.push(v);
        }
        // A shared generic entry's `kinds` word, its last filled parameter.
        words.extend(self.dict_values(&f.dicts)?);
        let h = self.b.ins_alloc(f.kind, words).map_err(mir)?;
        Ok(Val::V(self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![h]).map_err(mir)?))
    }

    /// The word a plain one-word local holds, as a `Local` read.
    #[track_caller]
    fn local_word(&mut self, local: HirLocal) -> Result<ValueId, Refusal> {
        // A two-slot local is one word, its box, wherever a word is
        // required (a capture).
        if let Some((lo, hi, lo_ty, hi_ty)) = self.pair_local(local) {
            let ty = self.hir.local(local).ty.clone().ok_or("capture pair type")?;
            let lo = self.b.use_local(lo, lo_ty).map_err(mir)?;
            let hi = self.b.use_local(hi, hi_ty).map_err(mir)?;
            return match self.box_pair_val(&ty, lo, hi)? {
                Val::V(v) => Ok(v),
                _ => Err("capture pair".into()),
            };
        }
        if let Some(&v) = self.aliases.get(&local.0) {
            return Ok(v);
        }
        if let Some(&slot) = self.boxes.get(&local.0) {
            return self.b.use_local(slot, MirTy::HeapRef).map_err(mir);
        }
        // A `()` local has no slot: the closure holds the empty tuple
        // object, as anywhere a word is stored (#2792).
        if self.unit_local(local) {
            return self.unit_object();
        }
        let ty = self.local_ty(local)?;
        let slot = self.slot(local)?;
        self.b.use_local(slot, ty).map_err(mir)
    }

    /// A trait method or operator on a bound type parameter, as HIR
    /// emission's `HirBuiltin::Bound` / `HirOp::Bound`: each operand's one
    /// word, then the dictionary parameter (and the method's own bounds'
    /// dictionaries), then the code pointer in its
    /// method slot through `CallIndirect`. A code pointer is an `i64`, no
    /// heap word.
    fn bound_call(&mut self, id: HirId, args: &[HirId], bound: &HirBoundCall) -> Result<Val, Refusal> {
        self.raw_host = false;
        let ty = self.expr_ty(id).ok_or("call type")?;
        // A two-word value comes back boxed, as the adapter's one-word ABI
        // returns it: the word, unboxed where a pair is read.
        let boxed_pair = self.pair_tys(id).is_some();
        if boxed_pair && !self.pair_unboxable(ty) {
            return Err("bound call pair".into());
        }
        let unit = is_unit(ty);
        let niche = if bound.boxed_ret { Some(self.word_ty(ty).ok_or("call type")?) } else { None };
        let ret = if unit {
            MirTy::I64
        } else if niche.is_some() || boxed_pair {
            MirTy::HeapRef
        } else {
            // An enum of no pair or niche layout comes back as its boxed
            // object (an open `Result<T, E>`, the instance's shared layout),
            // a heap tuple as its object (#2720), as from an existential's
            // slot.
            self.word_or_object(ty).ok_or_else(|| format!("bound call type {}", ty_kind(ty)))?
        };
        // Each argument's one word, a two-word value boxed, as the
        // adapter's one-word ABI takes it (#2718).
        let mut words = Vec::with_capacity(args.len() + 1);
        for &arg in args {
            let Some(v) = self.word_or_boxed(arg)? else {
                return Ok(Val::Never);
            };
            words.push(v);
        }
        let dict = match bound.forward {
            Some((index, own)) => {
                let at = words.len().checked_sub(1 + own as usize).ok_or("forwarded dictionary")?;
                let dict = self.b.ins_tuple_item(words[at], index, MirTy::HeapRef).map_err(mir)?;
                words[at] = dict;
                dict
            }
            None => {
                let dict = *self.dict_params.get(&bound.dict).ok_or("dictionary parameter")?;
                words.push(dict);
                words.extend(self.dict_values(&bound.dicts)?);
                dict
            }
        };
        let callee = self.b.ins_tuple_item(dict, bound.method, MirTy::I64).map_err(mir)?;
        let dest = self.at_expr(id, |l| l.b.ins_call_indirect(callee, words, ret).map_err(mir))?;
        if let Some(want) = niche {
            return self.unbox_niche(dest, want);
        }
        Ok(if unit { Val::Unit } else { Val::V(dest) })
    }

    /// Any pair [`Self::unbox_word`] reads back from its box: an enum, a
    /// product tuple (#1859), a range or a readonly class pair.
    fn pair_unboxable(&self, ty: &Ty) -> bool {
        self.cx.boxed_view(ty, None).is_some()
            || matches!(strip_readonly(ty), Ty::Tuple(items) if items.len() == 2)
            || crate::layout::two_word_range_kind(ty).is_some()
            || self.cx.pair_class(ty).is_some()
    }

    /// Copy-on-write of a bare-class existential `pack` of trait `class`,
    /// its `[boxed value, dictionary]` tuple (#2715). A method of the
    /// trait, called through the pack's slot, writes the boxed value, so
    /// the ops are the value's, where the dictionary's `kinds` word says it
    /// is a copy-on-write object (a scalar packs as its raw word): a share
    /// marks it, and a unique that copies it gives the place a pack of the
    /// copy (a tuple is immutable, and no copy-on-write object of its own).
    fn existential_cow(&mut self, id: HirId, pack: ValueId, unique: bool, class: &str) -> Result<Val, Refusal> {
        let index = self.cx.dict_kinds_index(class).ok_or("existential kinds word")?;
        let dict = self.b.ins_tuple_item(pack, 1, MirTy::HeapRef).map_err(mir)?;
        let kinds = self.b.ins_tuple_item(dict, index, MirTy::I64).map_err(mir)?;
        let one = self.b.ins_const(MirConst::I64(1)).map_err(mir)?;
        let bit = self.b.ins_binop(MirBinOp::BitAnd, kinds, one).map_err(mir)?;
        let zero = self.b.ins_const(MirConst::I64(0)).map_err(mir)?;
        let object = self.b.ins_cmp(MirCmpOp::Ne, bit, zero).map_err(mir)?;
        let (op, skip, join) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
        self.b.branch(object, op, skip).map_err(mir)?;
        self.switch(op);
        let value = self.b.ins_tuple_item(pack, 0, MirTy::HeapRef).map_err(mir)?;
        let out = self.at_expr(id, |l| l.b.ins_cow(value, unique).map_err(mir))?;
        let out = self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![out]).map_err(mir)?;
        if !unique {
            self.jump(join)?;
            self.switch(skip);
            self.jump(join)?;
            self.switch(join);
            return Ok(Val::V(pack));
        }
        // The copy is another object: the words differ (the op never collects).
        let (now, was) = (self.b.ins_word_as(out, MirTy::I64).map_err(mir)?, self.b.ins_word_as(value, MirTy::I64).map_err(mir)?);
        let same = self.b.ins_cmp(MirCmpOp::Eq, now, was).map_err(mir)?;
        let (copied, kept) = (self.b.create_block(), self.b.create_block());
        self.b.branch(same, kept, copied).map_err(mir)?;
        self.switch(kept);
        self.jump(join)?;
        self.switch(copied);
        let fresh = self.at_expr(id, |l| l.b.ins_alloc(MirAllocKind::TUPLE, vec![out, dict]).map_err(mir))?;
        let fresh = self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![fresh]).map_err(mir)?;
        let copied_from = self.cur;
        self.jump(join)?;
        self.switch(skip);
        self.jump(join)?;
        self.switch(join);
        Ok(Val::V(self.b.ins_stack_phi(vec![(kept, pack), (copied_from, fresh), (skip, pack)]).map_err(mir)?))
    }

    /// A trait method on a bare-class existential pack, as HIR emission's
    /// `HirBuiltin::Existential`: the pack's boxed value, the further
    /// arguments' words, the pack's dictionary, then the code pointer in
    /// the method's slot through `CallIndirect`. The slot holds what a
    /// bound's dictionary does (a raw method's `$dict` adapter), so a
    /// two-word result comes back boxed, as from [`Self::bound_call`], and
    /// a heap tuple or boxed enum as its object (#2684).
    fn existential_call(&mut self, id: HirId, args: &[HirId], slot: u32) -> Result<Val, Refusal> {
        self.raw_host = false;
        let ty = self.expr_ty(id).ok_or("call type")?;
        let boxed_pair = self.pair_tys(id).is_some();
        if boxed_pair && !self.pair_unboxable(ty) {
            return Err("existential call pair".into());
        }
        let unit = is_unit(ty);
        let ret = if unit {
            MirTy::I64
        } else if boxed_pair {
            MirTy::HeapRef
        } else {
            self.word_or_object(ty).ok_or_else(|| format!("existential call type {}", ty_kind(ty)))?
        };
        let (&recv, rest) = args.split_first().ok_or("existential call arity")?;
        let Some(pack) = self.word(recv)? else {
            return Ok(Val::Never);
        };
        if self.b.value_ty(pack) != MirTy::HeapRef {
            return Err("existential pack word".into());
        }
        // Each further argument's one word, a two-word value boxed, as
        // the slot's `$dict` adapter takes it (#2718).
        let mut rest_words = Vec::with_capacity(rest.len());
        for &arg in rest {
            let Some(v) = self.word_or_boxed(arg)? else {
                return Ok(Val::Never);
            };
            rest_words.push(v);
        }
        let field = |l: &mut Self, i: u32, ty: MirTy| -> Result<ValueId, Refusal> { l.b.ins_tuple_item(pack, i, ty).map_err(mir) };
        let value = field(self, 0, MirTy::HeapRef)?;
        let dict = field(self, 1, MirTy::HeapRef)?;
        let mut words = Vec::with_capacity(args.len() + 1);
        words.push(value);
        words.extend(rest_words);
        words.push(dict);
        let callee = self.b.ins_tuple_item(dict, slot, MirTy::I64).map_err(mir)?;
        let dest = self.at_expr(id, |l| l.b.ins_call_indirect(callee, words, ret).map_err(mir))?;
        Ok(if unit { Val::Unit } else { Val::V(dest) })
    }

    /// `lhs sym rhs` on two trait objects (#2740): their values compare
    /// through the left pack's dictionary slot `slot` (a superclass's
    /// `eq` .. `ge`) when both packs hold one instance's values, the same
    /// dictionary. Values of two instances are never equal (`==` false, `!=`
    /// true); an order between them panics with the site's message.
    fn existential_cmp(&mut self, id: HirId, sym: &str, lhs: HirId, rhs: HirId, slot: u32) -> Result<Val, Refusal> {
        self.raw_host = false;
        let (Some(l), Some(r)) = (self.word(lhs)?, self.word(rhs)?) else {
            return Ok(Val::Never);
        };
        if self.b.value_ty(l) != MirTy::HeapRef || self.b.value_ty(r) != MirTy::HeapRef {
            return Err("existential pack word".into());
        }
        let (lv, ld) = (self.b.ins_tuple_item(l, 0, MirTy::HeapRef).map_err(mir)?, self.b.ins_tuple_item(l, 1, MirTy::HeapRef).map_err(mir)?);
        let (rv, rd) = (self.b.ins_tuple_item(r, 0, MirTy::HeapRef).map_err(mir)?, self.b.ins_tuple_item(r, 1, MirTy::HeapRef).map_err(mir)?);
        let li = self.b.ins_cast(MirCastKind::Word, MirTy::I64, ld).map_err(mir)?;
        let ri = self.b.ins_cast(MirCastKind::Word, MirTy::I64, rd).map_err(mir)?;
        let same = self.b.ins_cmp(MirCmpOp::Eq, li, ri).map_err(mir)?;
        let out = self.temp();
        let (call, other, join) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
        self.b.branch(same, call, other).map_err(mir)?;
        self.switch(call);
        let callee = self.b.ins_tuple_item(ld, slot, MirTy::I64).map_err(mir)?;
        let v = self.at_expr(id, |l| l.b.ins_call_indirect(callee, vec![lv, rv, ld], MirTy::Bool).map_err(mir))?;
        self.b.def_local(out, v).map_err(mir)?;
        self.jump(join)?;
        self.switch(other);
        match sym {
            "==" | "!=" => {
                let v = self.b.ins_const(MirConst::Bool(sym == "!=")).map_err(mir)?;
                self.b.def_local(out, v).map_err(mir)?;
                self.jump(join)?;
            }
            _ => {
                let msg = self.site(id).string.ok_or("existential order message")?;
                let text = self.at_expr(id, |l| l.b.ins_string(msg).map_err(mir))?;
                self.at_expr(id, |l| l.b.panic(text).map_err(mir))?;
            }
        }
        self.switch(join);
        Ok(Val::V(self.b.use_local(out, MirTy::Bool).map_err(mir)?))
    }

    /// `f(args)` through a function value: each argument's one word (a
    /// two-word value boxed), then `f`, then `CallIndirect`, as HIR
    /// emission. A two-word result comes back boxed.
    fn call_value(&mut self, id: HirId, f: HirId, args: &[HirId]) -> Result<Val, Refusal> {
        // The result is one word either way.
        self.raw_host = false;
        let ty = self.expr_ty(id).ok_or("call type")?;
        let unit = is_unit(ty);
        // An enum of no pair or niche layout (an open `Result<int, E>`
        // a closure literal returns, #1520) is its object, as `call_poly`;
        // so is a heap tuple (`fn () use (s) => (s, s)`, #1524).
        let ret = if self.pair_tys(id).is_some()
            || (!unit && is_heap_tuple(ty))
            || (!unit && self.word_ty(ty).is_none() && self.cx.boxed_view(ty, None).is_some())
        {
            MirTy::HeapRef
        } else if unit {
            MirTy::I64
        } else {
            self.word_ty(ty).ok_or_else(|| format!("call value type {}", ty_kind(ty)))?
        };
        let mut words = Vec::with_capacity(args.len());
        for &arg in args {
            let Some(v) = self.word_or_boxed(arg)? else {
                return Ok(Val::Never);
            };
            words.push(v);
        }
        let Some(callee) = self.word(f)? else {
            return Ok(Val::Never);
        };
        if self.b.value_ty(callee) != MirTy::HeapRef {
            return Err("call value callee".into());
        }
        // A two-word result read as its two words: the callee's twin
        // returns them, or `unpair` reads them from its box.
        if let (Some(&unpair), Some(tys)) = (self.site(id).value_pair_call.as_ref(), self.pair_tys(id)) {
            let (lo, hi) = self.at_expr(id, |l| l.b.ins_call_indirect_pair(callee, words, tys, unpair).map_err(mir))?;
            if self.heap_pair(id) {
                self.b.mark_heap_pair_word(lo);
            }
            return Ok(Val::Pair(lo, hi));
        }
        let dest = self.at_expr(id, |l| l.b.ins_call_indirect(callee, words, ret).map_err(mir))?;
        Ok(if unit { Val::Unit } else { Val::V(dest) })
    }

    /// `f(args)` through a `PolyFn` local with no dictionary: each
    /// argument's word boxed to
    /// its tag, then `f`, `CallIndirect`, and the result unboxed when the
    /// function returns a bare type parameter.
    fn call_poly(&mut self, id: HirId, f: HirId, args: &[HirId], p: &HirPolyCall) -> Result<Val, Refusal> {
        self.raw_host = false;
        if p.words.len() != args.len() {
            return Err("polyfn call shape".into());
        }
        let ty = self.expr_ty(id).ok_or("call type")?;
        let unit = is_unit(ty);
        // A two-word result comes back as its object, unboxed where a pair
        // is read; a heap tuple is its object (#1511), as is an enum of no
        // pair or niche layout (#1515).
        let word = if unit {
            MirTy::I64
        } else if self.pair_tys(id).is_some()
            || is_heap_tuple(ty)
            || (self.word_ty(ty).is_none() && self.cx.boxed_view(ty, None).is_some())
        {
            MirTy::HeapRef
        } else {
            self.word_ty(ty).ok_or_else(|| format!("call value type {}", ty_kind(ty)))?
        };
        let mut words = Vec::with_capacity(args.len());
        for (&arg, &as_word) in args.iter().zip(&p.words) {
            // A `()` runs for its effect: to a bare type parameter the
            // word a dictionary adapter moves it as, else the empty
            // tuple object a plain call passes (#1450).
            if self.expr_ty(arg).is_some_and(|t| is_unit(t) && self.word_ty(t).is_none()) {
                if !self.effect(arg)? {
                    return Ok(Val::Never);
                }
                let v = if as_word { self.b.ins_const(MirConst::I64(0)).map_err(mir)? } else { self.unit_object()? };
                words.push(v);
                continue;
            }
            // A two-word value goes as its object, as a plain call passes
            // it to a shared body (#1479).
            let v = if self.pair_tys(arg).is_some() { self.boxed_word(arg)? } else { self.word(arg)? };
            let Some(v) = v else {
                return Ok(Val::Never);
            };
            // A bare type parameter takes the value's own word.
            let v = if as_word { self.b.ins_word(v).map_err(mir)? } else { v };
            words.push(v);
        }
        let dicts = p.dicts.len() as u32;
        let dicts = if dicts == 0 {
            0
        } else {
            let values = self.dict_values(&p.dicts)?;
            let n = values.len() as u32;
            words.extend(values);
            n
        };
        let Some(callee) = self.word(f)? else {
            return Ok(Val::Never);
        };
        if self.b.value_ty(callee) != MirTy::HeapRef {
            return Err("call value callee".into());
        }
        let ret = if p.word_ret || p.boxed_ret { MirTy::HeapRef } else { word };
        let dest = self.at_expr(id, |l| l.b.ins_call_indirect_dicts(callee, words, dicts, ret).map_err(mir))?;
        // An open `Option` / `Result` at a niche type: its enum object to
        // the niche word (#2723).
        if p.boxed_ret {
            if p.word_ret || p.ret_adapt.is_some() {
                return Err("call result niche".into());
            }
            return self.unbox_niche(dest, word);
        }
        // The shared body's tuple or enum, rebuilt at the call's types.
        let dest = match &p.ret_items {
            Some(items) if !p.word_ret && p.ret_adapt.is_none() && ret == MirTy::HeapRef => match self.at_expr(id, |l| l.unbox_ret_items(dest, items))? {
                Val::V(v) => v,
                _ => return Err("call result items".into()),
            },
            Some(_) => return Err("call result items".into()),
            None => dest,
        };
        // A bare type parameter's result comes back as its word.
        let dest = if p.word_ret { self.b.ins_word_as(dest, word).map_err(mir)? } else { dest };
        if let Some((target, operand)) = p.ret_adapt {
            if p.word_ret || ret != MirTy::HeapRef {
                return Err("call result adapter".into());
            }
            let f = self.at_expr(id, |l| l.b.ins_alloc(MirAllocKind::Fn { target, operand, mask: 0 }, vec![dest]).map_err(mir))?;
            return Ok(Val::V(self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![f]).map_err(mir)?));
        }
        Ok(if unit { Val::Unit } else { Val::V(dest) })
    }

    /// `lhs op rhs` through the operand type's instance method: each
    /// operand boxed (a shift count as its `int`), then the `CALL`, as HIR
    /// emission's `HirOp::Call`.
    fn op_call(&mut self, id: HirId, lhs: HirId, rhs: HirId, call: &HirOpCall) -> Result<Val, Refusal> {
        let site = &call.site;
        if site.pair_ret || site.coro || site.pair_args.len() != 2 {
            return Err("operator call shape".into());
        }
        let ret = self.expr_ty(id).and_then(|t| self.word_ty(t)).ok_or("operator result type")?;
        let mut words = Vec::with_capacity(2);
        for (i, operand) in [lhs, rhs].into_iter().enumerate() {
            if call.shift && i == 1 {
                let Some(v) = self.word(operand)? else {
                    return Ok(Val::Never);
                };
                if self.b.value_ty(v) != MirTy::I64 {
                    return Err("operator shift count".into());
                }
                words.push(v);
                continue;
            }
            // A heap carrier's boxed form is the word itself. A bare type
            // parameter of a shared body (`site.words`) takes any type's
            // own word, as a plain call passes it.
            let open = site.words.get(i).copied().unwrap_or(false);
            let ty = self.expr_ty(operand).ok_or("operator operand type")?;
            if !open && self.word_ty(ty).is_some_and(|t| t != MirTy::HeapRef) {
                return Err("operator operand word".into());
            }
            let Some(v) = self.boxed_word(operand)? else {
                return Ok(Val::Never);
            };
            if !open && self.b.value_ty(v) != MirTy::HeapRef {
                return Err("operator operand boxed".into());
            }
            words.push(self.b.ins_word(v).map_err(mir)?);
        }
        // The instance's dictionary, after the operands (#1661).
        if !site.dicts.is_empty() {
            let dicts = site.dicts.clone();
            words.extend(self.dict_values(&dicts)?);
        }
        let abi = CallAbi {
            params: words.iter().map(|&v| self.b.value_ty(v)).collect(),
            ret,
            ret_hi: None,
        };
        let (dest, _) = self.at_expr(id, |l| l.b.ins_call(site.target, words, &abi).map_err(mir))?;
        Ok(Val::V(dest))
    }

    /// `lhs op rhs` on a user type with no instance, as HIR emission's raw
    /// opcode: `==` / `!=` of two heap words (structural equality), or
    /// a number-backed scalar enum's backing words.
    fn op_prim(&mut self, id: HirId, sym: &str, lhs: HirId, rhs: HirId) -> Result<Val, Refusal> {
        // Two two-word operands compare on their words.
        if matches!(sym, "==" | "!=") && self.pair_eq_words(id, lhs, rhs).is_some() {
            return self.pair_eq(id, lhs, rhs, sym == "!=");
        }
        // Another two-word operand compares boxed, as emission pushes it.
        let (Some(l), Some(r)) = (self.word_or_boxed(lhs)?, self.word_or_boxed(rhs)?) else {
            return Ok(Val::Never);
        };
        let (lt, rt) = (self.b.value_ty(l), self.b.value_ty(r));
        if lt == MirTy::HeapRef && rt == MirTy::HeapRef && matches!(sym, "==" | "!=") {
            return self.structural_eq(l, r, sym == "!=");
        }
        // Niche `Option` / `Result` words compare on their words: the
        // same variant, then structural equality of the payloads.
        if lt == rt && matches!(lt, MirTy::NicheOpt | MirTy::NicheRes) && matches!(sym, "==" | "!=") {
            let layout = self.niche_layout(lhs, lt)?;
            return self.niche_eq(layout, l, r, sym == "!=");
        }
        // A string-backed scalar enum's (or a `string`'s) words order by
        // their bytes, as `<` on two strings does (#1722).
        if lt == MirTy::HeapRef && rt == MirTy::HeapRef {
            let cmp = match sym {
                "<" => MirCmpOp::Lt,
                ">" => MirCmpOp::Gt,
                "<=" => MirCmpOp::Le,
                ">=" => MirCmpOp::Ge,
                _ => return Err(format!("operator {sym} words")),
            };
            let order = self.b.ins_host_invoke(common::STRING_COMPARE_ID, vec![l, r]).map_err(mir)?;
            let zero = self.b.ins_const(MirConst::I64(0)).map_err(mir)?;
            return Ok(Val::V(self.b.ins_cmp(cmp, order, zero).map_err(mir)?));
        }
        // A `bool`-backed scalar enum's words compare as their `0` / `1`.
        if lt == MirTy::Bool && rt == MirTy::Bool && matches!(sym, "==" | "!=") {
            let l = self.b.ins_cast(MirCastKind::Word, MirTy::I64, l).map_err(mir)?;
            let r = self.b.ins_cast(MirCastKind::Word, MirTy::I64, r).map_err(mir)?;
            let cmp = if sym == "==" { MirCmpOp::Eq } else { MirCmpOp::Ne };
            return Ok(Val::V(self.b.ins_cmp(cmp, l, r).map_err(mir)?));
        }
        if lt != rt || !matches!(lt, MirTy::I64 | MirTy::F64) {
            return Err(format!("operator {sym} words"));
        }
        let cmp = match sym {
            "==" => MirCmpOp::Eq,
            "!=" => MirCmpOp::Ne,
            "<" => MirCmpOp::Lt,
            ">" => MirCmpOp::Gt,
            "<=" => MirCmpOp::Le,
            ">=" => MirCmpOp::Ge,
            _ => return Err(format!("operator {sym}")),
        };
        Ok(Val::V(self.b.ins_cmp(cmp, l, r).map_err(mir)?))
    }

    /// `resume h` / `resume h with v`: the coroutine runs to its next
    /// `yield`, whose word is the value. Read (`read`), a `0` word is the
    /// value nobody produced ([`Self::missing_value`]).
    fn resume(&mut self, id: HirId, handle: HirId, value: Option<HirId>, read: bool) -> Result<Val, Refusal> {
        let ty = self.expr_ty(id).ok_or("resume type")?.clone();
        let unit = is_unit(&ty);
        // A two-word value moves through the coroutine boxed.
        let pair = self.pair_tys(id);
        let dest_ty = match pair {
            _ if unit => MirTy::I64,
            Some(_) => MirTy::HeapRef,
            // A heap tuple or boxed enum moves as its object (#2680).
            None => self.word_or_object(&ty).ok_or("resume type")?,
        };
        let send = match value {
            Some(v) => match self.word_or_boxed(v)? {
                Some(w) => Some(w),
                None => return Ok(Val::Never),
            },
            None => self.missing_send(id)?,
        };
        let Some(h) = self.word(handle)? else {
            return Ok(Val::Never);
        };
        let out = self.b.ins_resume_coro(h, send, dest_ty).map_err(mir)?;
        if !read {
            return Ok(Val::Unit);
        }
        let Some(out) = self.missing_value(id, out)? else {
            return Ok(Val::Never);
        };
        match pair {
            _ if unit => Ok(Val::Unit),
            Some((lo_ty, hi_ty)) => self.unbox_word(&ty, out, lo_ty, hi_ty),
            None => Ok(Val::V(out)),
        }
    }

    /// `yield v` / `yield from h`: one instruction (the runtime saves the frame
    /// and resumes after it). The word
    /// moves boxed. A receiving yield's value is the word the next
    /// `resume .. with` sends.
    fn yield_(&mut self, id: HirId, value: HirId, from: bool, receive: bool) -> Result<Val, Refusal> {
        let ty = self.expr_ty(id).ok_or("yield type")?.clone();
        let receive = receive && !from && !is_unit(&ty);
        let recv_ty = if !receive {
            None
        } else if self.pair_tys(id).is_some() {
            Some(MirTy::HeapRef)
        } else {
            Some(self.word_or_object(&ty).ok_or("yield type")?)
        };
        let Some(v) = self.word_or_boxed(value)? else {
            return Ok(Val::Never);
        };
        let out = self.b.ins_yield(v, from, recv_ty).map_err(mir)?;
        if !receive {
            return Ok(Val::Unit);
        }
        let Some(out) = self.missing_value(id, out)? else {
            return Ok(Val::Never);
        };
        match self.pair_tys(id) {
            Some((lo_ty, hi_ty)) => self.unbox_word(&ty, out, lo_ty, hi_ty),
            None => Ok(Val::V(out)),
        }
    }

    /// What a `resume` without `with` or a `for` (`id`) sends: nothing, or
    /// the word standing for no value at a receive type `0` is a value of.
    fn missing_send(&mut self, id: HirId) -> Result<Option<ValueId>, Refusal> {
        match self.site(id).missing_send {
            Some(word) => Ok(Some(self.b.ins_const(MirConst::I64(word)).map_err(mir)?)),
            None => Ok(None),
        }
    }

    /// A gen fn's completion with no value (`return;`, its body's end): the
    /// word `0`, or the one its site plans for a yield type `0` is a value
    /// of (#2717).
    fn ret_missing(&mut self, id: HirId) -> Result<(), Refusal> {
        let Some(word) = self.site(id).missing.filter(|_| self.hir.is_coro) else {
            return self.ret_zero();
        };
        let ty = self.b.func().ret_ty.ok_or("return value")?;
        let w = self.b.ins_const(MirConst::I64(word)).map_err(mir)?;
        let w = self.fit(w, ty)?;
        self.ret_word(w)
    }

    /// The word `w` a `resume` or binding `yield` (`id`) read, where a `0`
    /// is the value nobody produced (#2717): the type's zero (its `zero`
    /// node, an object) in its place, or, for a type with no zero, the panic
    /// the site's message names. A zero that is the word `0` itself, and a
    /// type variable's word, stay as they are.
    fn missing_value(&mut self, id: HirId, w: ValueId) -> Result<Option<ValueId>, Refusal> {
        let zero = match self.hir.expr(id).kind {
            HirKind::Resume { zero, .. } | HirKind::Yield { zero, .. } => zero,
            _ => None,
        };
        let msg = self.site(id).string;
        if zero.is_none() && msg.is_none() {
            return Ok(Some(w));
        }
        let ty = self.b.value_ty(w);
        let bits = if ty == MirTy::I64 { w } else { self.b.ins_cast(MirCastKind::Word, MirTy::I64, w).map_err(mir)? };
        let none = self.b.ins_const(MirConst::I64(self.site(id).missing.unwrap_or(0))).map_err(mir)?;
        let missing = self.b.ins_cmp(MirCmpOp::Eq, bits, none).map_err(mir)?;
        let (fill, join) = (self.b.create_block(), self.b.create_block());
        let Some(zero) = zero else {
            let msg = msg.ok_or("missing value message")?;
            self.b.branch(missing, fill, join).map_err(mir)?;
            self.switch(fill);
            let text = self.at_expr(id, |l| l.b.ins_string(msg).map_err(mir))?;
            self.at_expr(id, |l| l.b.panic(text).map_err(mir))?;
            self.switch(join);
            return Ok(Some(w));
        };
        let t = self.temp();
        self.b.def_local(t, w).map_err(mir)?;
        self.b.branch(missing, fill, join).map_err(mir)?;
        self.switch(fill);
        let Some(z) = self.word_or_boxed(zero)? else {
            return Err("missing value zero".into());
        };
        let z = self.fit(z, ty)?;
        self.b.def_local(t, z).map_err(mir)?;
        self.jump(join)?;
        self.switch(join);
        Ok(Some(self.b.use_local(t, ty).map_err(mir)?))
    }

    /// HIR `Clear` of `local` past its last use, as HIR emission's zero
    /// stores: each of its words that may hold a heap object is zeroed
    /// (`ClearWord`), so the frame no longer roots it. A scalar's word
    /// roots nothing. A local some path here never bound has nothing to
    /// clear on that path, and is left as it is.
    fn clear(&mut self, local: HirLocal) -> Result<(), Refusal> {
        // Every word the local was bound to: a boxed stack array's object,
        // else its element or field slots, a pair's payload word, or its
        // own slot.
        let words: Vec<(LocalId, MirTy)> = if let Some(&slot) = self.boxes.get(&local.0) {
            vec![(slot, MirTy::HeapRef)]
        } else if let Some((base, n, ty)) = self.stack(local.0) {
            (0..n as u32).map(|i| (LocalId(base + i), ty)).collect()
        } else if let Some(tys) = self.sroa(local.0) {
            let base = self.slot(local)?.0;
            tys.iter().enumerate().map(|(i, &ty)| (LocalId(base + i as u32), ty)).collect()
        } else if self.pair_local(local).is_some()
            && self.hir.local(local).ty.as_ref().is_some_and(crate::hir::classify::is_range_pair)
        {
            // An unboxed range is two numbers: nothing to clear.
            return Ok(());
        } else if let Some((lo, _, lo_ty, _)) = self.pair_local(local) {
            // A heap payload moves through a pair as an opaque word.
            vec![(lo, if lo_ty == MirTy::I64 { MirTy::HeapRef } else { lo_ty })]
        } else if self.pair_local_tys(local).is_some() {
            return Err("clear pair local".into());
        } else if self.frame.slot(local).is_none() && !self.defer_locals.contains_key(&local.0) {
            // No frame slot: a binding read as its value (an arm's SSA
            // value, which need not reach this clear), or one emission
            // never bound (an arm a split enum dropped or renamed to its
            // field). No frame slot holds it.
            return Ok(());
        } else {
            vec![(self.slot(local)?, self.local_ty(local)?)]
        };
        for (slot, ty) in words {
            if !ty.is_heap_word() || !self.b.local_defined(slot) {
                continue;
            }
            let v = self.b.use_local(slot, ty).map_err(mir)?;
            self.b.ins_clear_word(v).map_err(mir)?;
        }
        Ok(())
    }

    /// Fill the slots of stack array or class local `local` from `init`:
    /// each item or field into its own slot, or another stack array's slots
    /// copied.
    fn fill_slots(&mut self, local: HirLocal, init: HirId) -> Result<Val, Refusal> {
        if self.boxes.contains_key(&local.0) {
            return Err("refill boxed stack array".into());
        }
        let base = self.slot(local)?.0;
        let tys = match (self.stack(local.0), self.sroa(local.0)) {
            (Some((_, n, ty)), _) => vec![ty; n],
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
                    // A two-word item's slot holds its box.
                    let niche = self.expr_ty(arg).and_then(|t| self.word_ty(t)).filter(|w| matches!(w, MirTy::NicheOpt | MirTy::NicheRes));
                    let v = match niche {
                        // A slot of a generic class's field open in a
                        // parameter holds a niche `Option` / `Result` boxed.
                        Some(niche) if ty == MirTy::HeapRef => {
                            let layout = self.niche_layout(arg, niche)?;
                            self.boxed_niche_word(arg, layout)?
                        }
                        _ if ty == MirTy::HeapRef => self.word_or_boxed(arg)?,
                        _ => self.word(arg)?,
                    };
                    let Some(v) = v else {
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
            HirKind::Local(src) if let Some((from, n, ty)) = self.stack(src.0) && n == tys.len() => {
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
        if !query::stack_index_proven(self.hir, node, index, n) {
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
        if let Some(m) = query::index_euclid(self.hir, index) {
            let sh = self.b.ins_const(MirConst::I64(63)).map_err(mir)?;
            let m = self.b.ins_const(MirConst::I64(i64::from(m))).map_err(mir)?;
            let sign = self.b.ins_binop(MirBinOp::Shr, idx, sh).map_err(mir)?;
            let fix = self.b.ins_binop(MirBinOp::BitAnd, sign, m).map_err(mir)?;
            idx = self.b.ins_binop(MirBinOp::Add, idx, fix).map_err(mir)?;
        }
        Ok(Some(idx))
    }

    /// A heap local's new word from `value`: [`Self::boxed_word`], or a
    /// fixed array read from another local copied (`vec_from_array`), as
    /// the two never share elements.
    #[track_caller]
    fn boxed_word_copied(&mut self, value: HirId) -> Result<Option<ValueId>, Refusal> {
        let Some(native) = self.cx.array_copy(value) else {
            return self.boxed_word(value);
        };
        let Some(v) = self.word(value)? else {
            return Ok(None);
        };
        let layout = common::HOST_ENUM_LAYOUT_BOXED as u8;
        Ok(Some(self.at_expr(value, |l| l.b.ins_host_invoke_as(native, vec![v], layout, Some(MirTy::HeapRef)).map_err(mir))?))
    }

    /// The word type of a static `id` names: as a local of its type. A
    /// two-word static is one boxed word.
    fn static_ty(&self, id: HirId) -> Result<MirTy, Refusal> {
        if self.pair_tys(id).is_some() {
            return Ok(MirTy::HeapRef);
        }
        let ty = self.expr_ty(id).ok_or("static type")?;
        self.word_ty(ty)
            .or_else(|| self.cx.boxed_view(ty, None).map(|_| MirTy::HeapRef))
            .or_else(|| matches!(strip_readonly(ty), Ty::Tuple(items) if !items.is_empty()).then_some(MirTy::HeapRef))
            // A `()` static holds the empty tuple object (#1427, #2562).
            .or_else(|| is_unit(ty).then_some(MirTy::HeapRef))
            .ok_or_else(|| format!("static type {}", ty_kind(ty)))
    }

    /// A `Global` read: a static's word (`LoadStatic`) or a folded
    /// `const`. A function used as a value is not this phase's.
    fn global(&mut self, id: HirId) -> Result<Val, Refusal> {
        use crate::const_fold::ConstValue;
        if let Some(slot) = self.cx.static_slot(id) {
            // A two-word static's words, from its slot and its tail.
            if let (Some(tail), Some((lo_ty, hi_ty))) = (self.cx.static_tail(id), self.pair_tys(id)) {
                let lo = self.b.ins_load_static(slot, lo_ty).map_err(mir)?;
                let hi = self.b.ins_load_static(tail, hi_ty).map_err(mir)?;
                return Ok(Val::Pair(lo, hi));
            }
            let ty = self.static_ty(id)?;
            let v = self.b.ins_load_static(slot, ty).map_err(mir)?;
            // A two-word static read as its pair, as HIR emission unboxes it.
            if let Some((lo_ty, hi_ty)) = self.pair_tys(id) {
                let ty = self.expr_ty(id).ok_or("static type")?.clone();
                return self.unbox_word(&ty, v, lo_ty, hi_ty);
            }
            return Ok(Val::V(v));
        }
        let c = match self.cx.const_value(id) {
            Some(ConstValue::Int(n)) => MirConst::I64(*n),
            Some(ConstValue::Float(f)) => MirConst::F64(f.to_bits()),
            Some(ConstValue::Bool(b)) => MirConst::Bool(*b),
            Some(ConstValue::Str(_)) => match self.site(id).string.as_ref() {
                Some(&idx) if self.expr_ty(id).is_some_and(is_string) => return Ok(Val::V(self.b.ins_string(idx).map_err(mir)?)),
                _ => return Err("const string".into()),
            },
            None => return Err("global".into()),
        };
        if self.expr_ty(id).and_then(scalar) != Some(c.ty()) {
            return Err("const type".into());
        }
        Ok(Val::V(self.b.ins_const(c).map_err(mir)?))
    }

    /// `x++` / `--x` on a local or static used for its value: the old
    /// word, then `x ± 1` stored, the value the old (postfix) or new
    /// (prefix) word, as HIR emission's `INC` / `DEC` on the slot.
    fn adjust_value(&mut self, id: HirId, place: HirId, value: HirId) -> Result<Val, Refusal> {
        let prefix = self.hir.expr(id).flags.contains(HirFlags::PREFIX);
        if matches!(self.hir.expr(place).kind, HirKind::Field { .. } | HirKind::Index { .. }) {
            return self.adjust_elem(id, place, value, prefix);
        }
        let Some(old) = self.word(place)? else {
            return Ok(Val::Never);
        };
        let Some(new) = self.word(value)? else {
            return Ok(Val::Never);
        };
        let ty = self.b.value_ty(old);
        if !matches!(ty, MirTy::I64 | MirTy::F64) || self.b.value_ty(new) != ty {
            return Err("adjust type".into());
        }
        match self.hir.expr(place).kind {
            HirKind::Global { .. } => {
                let slot = self.cx.static_slot(place).ok_or("adjust place")?;
                if self.static_ty(place)? != ty {
                    return Err("adjust type".into());
                }
                self.b.ins_store_static(slot, new).map_err(mir)?;
            }
            HirKind::Local(local) if self.pair_local(local).is_none() => {
                if self.local_ty(local)? != ty {
                    return Err("adjust type".into());
                }
                let slot = self.slot(local)?;
                self.b.def_local(slot, new).map_err(mir)?;
            }
            _ => return Err("adjust place".into()),
        }
        Ok(Val::V(if prefix { new } else { old }))
    }

    /// `xs[i]++` / `--b.v` used for its value: the element or field read
    /// once (the old word), `old ± 1` stored as the statement `x = x ± 1`
    /// would, and the old (postfix) or new (prefix) word. The place's
    /// operands are read again by the store, so they must be pure.
    fn adjust_elem(&mut self, id: HirId, place: HirId, value: HirId, prefix: bool) -> Result<Val, Refusal> {
        let HirKind::Bin { lhs: read, .. } = self.hir.expr(value).kind else {
            return Err("adjust place".into());
        };
        // `xs[0].v++` and `g[r][0]++` read their base twice, as `+=` does
        // (#1978, #2661).
        let repeatable = match self.hir.expr(place).kind {
            HirKind::Field { base, .. } => query::read_base(self.hir, base),
            HirKind::Index { base, index, .. } => query::read_base(self.hir, base) && query::pure_index(self.hir, index),
            _ => false,
        };
        if !repeatable {
            return Err("adjust place".into());
        }
        let Some(old) = self.word(read)? else {
            return Ok(Val::Never);
        };
        self.preset.insert(read.0, Val::V(old));
        let new = self.word(value);
        self.preset.remove(&read.0);
        let Some(new) = new? else {
            return Ok(Val::Never);
        };
        let ty = self.b.value_ty(old);
        if !matches!(ty, MirTy::I64 | MirTy::F64) || self.b.value_ty(new) != ty {
            return Err("adjust type".into());
        }
        self.preset.insert(value.0, Val::V(new));
        let prev = self.adjust_store.replace(id);
        let stored = self.value(id);
        self.adjust_store = prev;
        self.preset.remove(&value.0);
        if matches!(stored?, Val::Never) {
            return Ok(Val::Never);
        }
        Ok(Val::V(if prefix { new } else { old }))
    }

    /// `STATIC = v`: the value, then `StoreStatic`, as HIR emission. The
    /// value is stored as is (no fixed-array copy).
    fn store_static(&mut self, place: HirId, value: HirId) -> Result<Val, Refusal> {
        let slot = self.cx.static_slot(place).ok_or("assign place")?;
        if let (Some(tail), Some((lo_ty, hi_ty))) = (self.cx.static_tail(place), self.pair_tys(place)) {
            let Some((lo, hi)) = self.pair(value)? else {
                return Ok(Val::Never);
            };
            if self.b.value_ty(lo) != lo_ty || self.b.value_ty(hi) != hi_ty {
                return Err("assign pair static type".into());
            }
            self.b.ins_store_static(slot, lo).map_err(mir)?;
            self.b.ins_store_static(tail, hi).map_err(mir)?;
            return Ok(Val::Unit);
        }
        let ty = self.static_ty(place)?;
        let Some(v) = (if ty == MirTy::HeapRef { self.word_or_boxed(value)? } else { self.word(value)? }) else {
            return Ok(Val::Never);
        };
        if self.b.value_ty(v) != ty {
            return Err("assign type".into());
        }
        self.b.ins_store_static(slot, v).map_err(mir)?;
        Ok(Val::Unit)
    }

    /// Whether `id` is a heap array this phase lowers (its type is one word).
    /// The word a boxed tuple's field of type `ty` holds: a scalar or a
    /// plain heap word.
    fn tuple_item(&self, ty: &Ty) -> Option<MirTy> {
        scalar(ty)
            // A scalar enum item is its backing word (#1903).
            .or_else(|| self.cx.scalar_enum(ty))
            // A `()` item is the empty tuple object (#1427).
            .or_else(|| is_unit(ty).then_some(MirTy::HeapRef))
            .or_else(|| self.plain_heap(ty).then_some(MirTy::HeapRef))
            // A type parameter's item in a shared body: the opaque word as
            // stored, as an array's (#1143).
            .or_else(|| (self.opaque_vars && is_var(ty)).then_some(MirTy::I64))
            // A two-word item (an `Option<int>`, a range) rides boxed (#1486).
            .or_else(|| self.boxed_pair_item(ty).then_some(MirTy::HeapRef))
            // A niche `Option` / `Result` item is its niche word, as
            // `make_tuple` stores it (#1614).
            .or_else(|| self.cx.niche(ty))
    }

    /// Whether a tuple item of type `ty` is a two-word value held as its
    /// box, as `word_or_boxed` stores it.
    fn boxed_pair_item(&self, ty: &Ty) -> bool {
        self.word_ty(ty).is_none()
            && (self.cx.boxed_view(ty, None).is_some() || crate::layout::two_word_range_kind(ty).is_some())
    }

    fn heap_array(&self, id: HirId) -> bool {
        self.lane(id).is_some() || self.expr_ty(id).is_some_and(|t| self.word_ty(t) == Some(MirTy::HeapRef) && self.elem_word(t).is_some())
    }

    /// A `Vec` method on lane array `args[0]`, or a constructor of one,
    /// written out over the lane ops ([`HirLaneCall`]).
    fn lane_call(&mut self, id: HirId, args: &[HirId], call: HirLaneCall) -> Result<Val, Refusal> {
        match call {
            HirLaneCall::New | HirLaneCall::WithCapacity => {
                let lane = self.lane(id).ok_or("lane constructor")?;
                let n = match (call, args) {
                    (HirLaneCall::WithCapacity, [n]) => match self.word(*n)? {
                        Some(n) => Some(n),
                        None => return Ok(Val::Never),
                    },
                    _ => None,
                };
                let arr = self.at_expr(id, |l| l.b.ins_alloc(MirAllocKind::Lanes { tag_mask: lane.shape.tag_mask }, Vec::new()).map_err(mir))?;
                let arr = self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![arr]).map_err(mir)?;
                if let Some(n) = n {
                    let reserve = common::host_native_id("vec_reserve").ok_or("vec_reserve")? as u16;
                    self.at_expr(id, |l| l.b.ins_host_invoke(reserve, vec![arr, n]).map_err(mir))?;
                }
                Ok(Val::V(arr))
            }
            HirLaneCall::Pop | HirLaneCall::Remove => self.lane_take(id, args),
            HirLaneCall::Insert => self.lane_insert(id, args),
            HirLaneCall::Thunk => Err("lane thunk call".into()),
        }
    }

    /// The element type of lane array `id` (a `Vec` or a fixed array).
    fn lane_elem_ty(&self, id: HirId) -> Result<Ty, Refusal> {
        match self.expr_ty(id).map(strip_readonly) {
            Some(Ty::Array { element, .. }) => Ok(element.as_ref().clone()),
            Some(t) => vec_element_ty(t).cloned().ok_or_else(|| "lane element type".into()),
            None => Err("lane element type".into()),
        }
    }

    /// `v.pop()` or `v.remove(i)` on lane array `args[0]`: `None` when
    /// there is no such element; else the element's words are read and
    /// boxed, `vec_drain` takes it out of the array (the ones after it
    /// move down), and the result is `Some` of the box in the call's
    /// layout (a niche word, or the enum object a `Vec` thunk returns).
    fn lane_take(&mut self, id: HirId, args: &[HirId]) -> Result<Val, Refusal> {
        let raw = std::mem::take(&mut self.raw_host);
        let lane = self.lane(args[0]).ok_or("lane receiver")?;
        let elem = self.lane_elem_ty(args[0])?;
        let ty = self.expr_ty(id).ok_or("lane call type")?.clone();
        let niche = !raw && self.word_ty(&ty) == Some(MirTy::NicheOpt);
        let ret = if niche { MirTy::NicheOpt } else { MirTy::HeapRef };
        let Some(arr) = self.word(args[0])? else {
            return Ok(Val::Never);
        };
        let at = match args.get(1) {
            Some(&i) => match self.index_word(i)? {
                Some(i) => Some(i),
                None => return Ok(Val::Never),
            },
            None => None,
        };
        let out = self.temp();
        let (some_b, none_b, join) = (self.b.create_block(), self.b.create_block(), self.b.create_block());
        self.at_expr(id, |l| {
            let n = l.b.ins_array_len(arr).map_err(mir)?;
            let zero = l.b.ins_const(MirConst::I64(0)).map_err(mir)?;
            let one = l.b.ins_const(MirConst::I64(1)).map_err(mir)?;
            // The element taken: the last one, or `i` when `0 <= i < n`.
            let i = match at {
                None => {
                    let empty = l.b.ins_cmp(MirCmpOp::Le, n, zero).map_err(mir)?;
                    l.b.branch(empty, none_b, some_b).map_err(mir)?;
                    l.switch(some_b);
                    l.b.ins_binop(MirBinOp::Sub, n, one).map_err(mir)?
                }
                Some(i) => {
                    let check_hi = l.b.create_block();
                    let below = l.b.ins_cmp(MirCmpOp::Lt, i, zero).map_err(mir)?;
                    l.b.branch(below, none_b, check_hi).map_err(mir)?;
                    l.switch(check_hi);
                    let above = l.b.ins_cmp(MirCmpOp::Ge, i, n).map_err(mir)?;
                    l.b.branch(above, none_b, some_b).map_err(mir)?;
                    l.switch(some_b);
                    i
                }
            };
            let (lo, hi) = l.b.ins_lane_index(arr, i, lane.lo, lane.hi).map_err(mir)?;
            // Boxed before the array drops it: a heap payload is then held
            // by its box, a reference.
            let Val::V(boxed) = l.box_pair_val(&elem, lo, hi)? else {
                return Err("lane element box".into());
            };
            let next = l.b.ins_binop(MirBinOp::Add, i, one).map_err(mir)?;
            l.b.ins_host_invoke(common::VEC_DRAIN_ID, vec![arr, i, next]).map_err(mir)?;
            let some = if niche {
                l.b.ins_cast(MirCastKind::Word, MirTy::NicheOpt, boxed).map_err(mir)?
            } else {
                l.make_enum(1, Some(boxed))?
            };
            l.b.def_local(out, some).map_err(mir)?;
            l.jump(join)?;
            l.switch(none_b);
            let none = if niche {
                l.b.ins_cast(MirCastKind::Word, MirTy::NicheOpt, zero).map_err(mir)?
            } else {
                l.make_enum(0, None)?
            };
            l.b.def_local(out, none).map_err(mir)?;
            l.jump(join)?;
            l.switch(join);
            Ok(())
        })?;
        Ok(Val::V(self.b.use_local(out, ret).map_err(mir)?))
    }

    /// `v.insert(i, x)` on lane array `args[0]`: unless `0 <= i <= len`
    /// the runtime's `vec_insert` panics, as on any array; else `x`'s
    /// words are pushed and, unless `i` is the end, `vec_rotate_in` moves
    /// them to `i` (the elements from `i` up one).
    fn lane_insert(&mut self, id: HirId, args: &[HirId]) -> Result<Val, Refusal> {
        let lane = self.lane(args[0]).ok_or("lane receiver")?;
        let Some(arr) = self.word(args[0])? else {
            return Ok(Val::Never);
        };
        let Some(i) = self.index_word(args[1])? else {
            return Ok(Val::Never);
        };
        let Some((lo, hi)) = self.pair(args[2])? else {
            return Ok(Val::Never);
        };
        let insert = common::host_native_id("vec_insert").ok_or("vec_insert")? as u16;
        let grown = self.at_expr(id, |l| {
            let n = l.b.ins_array_len(arr).map_err(mir)?;
            let zero = l.b.ins_const(MirConst::I64(0)).map_err(mir)?;
            let (check_hi, oob, inside, rotate, done) = (l.b.create_block(), l.b.create_block(), l.b.create_block(), l.b.create_block(), l.b.create_block());
            let below = l.b.ins_cmp(MirCmpOp::Lt, i, zero).map_err(mir)?;
            l.b.branch(below, oob, check_hi).map_err(mir)?;
            l.switch(check_hi);
            let above = l.b.ins_cmp(MirCmpOp::Gt, i, n).map_err(mir)?;
            l.b.branch(above, oob, inside).map_err(mir)?;
            // Out of range: the native panics with its own message.
            let start = l.order.len();
            l.switch(oob);
            l.b.ins_host_invoke(insert, vec![arr, i, zero]).map_err(mir)?;
            l.keep_pins()?;
            l.b.unreachable().map_err(mir)?;
            l.mark_cold(start);
            l.switch(inside);
            let out = l.b.ins_lane_push(arr, lo, hi, lane.shape).map_err(mir)?;
            let out = l.b.ins_gc_barrier(MirGcKind::Safepoint, vec![out]).map_err(mir)?;
            let end = l.b.ins_cmp(MirCmpOp::Eq, i, n).map_err(mir)?;
            l.b.branch(end, done, rotate).map_err(mir)?;
            l.switch(rotate);
            l.b.ins_host_invoke(common::VEC_ROTATE_IN_ID, vec![out, i]).map_err(mir)?;
            l.jump(done)?;
            l.switch(done);
            Ok(out)
        })?;
        self.pushed(args[0], grown)?;
        Ok(Val::Unit)
    }

    /// `xs[i]` of lane array `base`: the element's two words.
    fn lane_index(&mut self, base: HirId, index: HirId, lane: HirLaneElem) -> Result<Val, Refusal> {
        let Some(arr) = self.word(base)? else {
            return Ok(Val::Never);
        };
        let Some(idx) = self.index_word(index)? else {
            return Ok(Val::Never);
        };
        let (lo, hi) = self.b.ins_lane_index(arr, idx, lane.lo, lane.hi).map_err(mir)?;
        Ok(Val::Pair(lo, hi))
    }

    /// `xs[i]` on a heap array: the array, then the index, as HIR emission
    /// pushes them.
    fn heap_index(&mut self, id: HirId, base: HirId, index: HirId) -> Result<Val, Refusal> {
        // A tuple's `()` item: the base and index for their effects (#1427).
        if self.expr_ty(id).is_some_and(is_unit) {
            if !self.effect(base)? || !self.effect(index)? {
                return Ok(Val::Never);
            }
            return Ok(Val::Unit);
        }
        if let Some(lane) = self.lane(base) {
            return self.lane_index(base, index, lane);
        }
        let ty = match self.expr_ty(base) {
            // A tuple's field: a scalar.
            Some(t) if matches!(strip_readonly(t), Ty::Tuple(_)) => self.expr_ty(id).and_then(|t| self.tuple_item(t)),
            Some(t) => self.elem_word(t),
            None => None,
        }
        .ok_or("element type")?;
        // A two-word tuple local read at a runtime index: its box.
        let Some(arr) = self.word_or_boxed(base)? else {
            return Ok(Val::Never);
        };
        // A tuple's item at a constant index: one slot read.
        if self.expr_ty(base).is_some_and(|t| matches!(strip_readonly(t), Ty::Tuple(_)))
            && let HirKind::Lit(Lit::Int(k)) = self.hir.expr(index).kind
            && let Ok(k) = u32::try_from(k)
        {
            return Ok(Val::V(self.b.ins_tuple_item(arr, k, ty).map_err(mir)?));
        }
        let Some(idx) = self.index_word(index)? else {
            return Ok(Val::Never);
        };
        // A boxed stack array's read is always checked, as HIR emission's.
        let proven = self.hir.expr(id).flags.contains(HirFlags::IN_BOUNDS) && self.boxed(base).is_none();
        let byte = self.expr_ty(base).and_then(array_elem).is_some_and(is_byte);
        let v = self.b.ins_index(arr, idx, ty, proven).map_err(mir)?;
        if byte {
            self.b.mark_byte_load(v);
        }
        Ok(Val::V(v))
    }

    /// `s[i]` on a string, as HIR emission's: `string_byte_at`, which
    /// answers `-1` out of range, and there `panic "index out of bounds"`
    /// at the statement's location, the message emission interned.
    fn string_index(&mut self, id: HirId, base: HirId, index: HirId) -> Result<Val, Refusal> {
        let native = super::host_allow::host_edge_spec_by_name("string_byte_at").ok_or("string index native")?.id;
        let &idx = self.site(id).string.as_ref().ok_or("string index message")?;
        let Some(s) = self.word(base)? else {
            return Ok(Val::Never);
        };
        let Some(i) = self.index_word(index)? else {
            return Ok(Val::Never);
        };
        if self.b.value_ty(s) != MirTy::HeapRef {
            return Err("string index base".into());
        }
        let layout = common::HOST_ENUM_LAYOUT_BOXED as u8;
        let byte = self.at_expr(id, |l| l.b.ins_host_invoke_as(native, vec![s, i], layout, Some(MirTy::I64)).map_err(mir))?;
        let zero = self.b.ins_const(MirConst::I64(0)).map_err(mir)?;
        let out = self.b.ins_cmp(MirCmpOp::Lt, byte, zero).map_err(mir)?;
        let (oob, ok) = (self.b.create_block(), self.b.create_block());
        self.b.branch(out, oob, ok).map_err(mir)?;
        self.switch(oob);
        let msg = self.b.ins_string(idx).map_err(mir)?;
        self.b.panic(msg).map_err(mir)?;
        self.switch(ok);
        Ok(Val::V(byte))
    }

    /// `xs[i] = v` on a heap array: the value first, then the array and
    /// index, as HIR emission stages them. The bounds check stays unless
    /// the checker proved the index and the value cannot change the array.
    fn heap_store(&mut self, place: HirId, base: HirId, index: HirId, value: HirId) -> Result<Val, Refusal> {
        if self.lane(base).is_some() {
            let Some((lo, hi)) = self.pair(value)? else {
                return Ok(Val::Never);
            };
            let Some(arr) = self.word(base)? else {
                return Ok(Val::Never);
            };
            let Some(idx) = self.index_word(index)? else {
                return Ok(Val::Never);
            };
            self.b.ins_lane_store(arr, idx, lo, hi, self.lane(base).ok_or("lane array")?.shape).map_err(mir)?;
            return Ok(Val::Unit);
        }
        let ty = self.expr_ty(base).and_then(|t| self.elem_word(t)).ok_or("element type")?;
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
        self.hir.visit(value, &mut |e| {
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
    /// is the safepoint's. Heap elements are boxed words; the array carries
    /// its element stamp.
    fn make_array(&mut self, id: HirId, args: &[HirId]) -> Result<Val, Refusal> {
        if let Some(lane) = self.lane(id) {
            let mut words = Vec::with_capacity(args.len() * 2);
            for &arg in args {
                let Some((lo, hi)) = self.pair(arg)? else {
                    return Ok(Val::Never);
                };
                words.extend([lo, hi]);
            }
            let arr = self.b.ins_alloc(MirAllocKind::Lanes { tag_mask: lane.shape.tag_mask }, words).map_err(mir)?;
            return Ok(Val::V(self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![arr]).map_err(mir)?));
        }
        let ty = self
            .expr_ty(id)
            .and_then(|t| array_elem(t).and_then(scalar).or_else(|| self.elem_word(t)))
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
        // Stamped as HIR emission stamps it: packed bytes / bools, pointer
        // elements.
        let stamp = self.expr_ty(id).and_then(array_elem).map_or(0, |e| self.cx.array_stamp(e));
        let arr = self.b.ins_alloc_kinds(MirAllocKind::Array, items, stamp).map_err(mir)?;
        Ok(Val::V(self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![arr]).map_err(mir)?))
    }

    /// A local `const` table of literals (#1561): built into static `slot`
    /// the first time it runs in this runtime (an isolate thread's statics start
    /// zeroed; a shared-heap job's copy may not have it yet), loaded after
    /// that. No code writes the array, so sharing it is not seen.
    fn frozen_array(&mut self, id: HirId, args: &[HirId], slot: u32) -> Result<Val, Refusal> {
        let have = self.b.ins_load_static(slot, MirTy::HeapRef).map_err(mir)?;
        let zero = self.b.ins_const(MirConst::I64(0)).map_err(mir)?;
        let unset = self.b.ins_cmp(MirCmpOp::Eq, have, zero).map_err(mir)?;
        let build = self.b.create_block();
        let loaded = self.b.create_block();
        let join = self.b.create_block();
        self.b.branch(unset, build, loaded).map_err(mir)?;
        self.switch(build);
        let Val::V(built) = self.make_array(id, args)? else {
            return Err("table element".into());
        };
        self.b.ins_store_static(slot, built).map_err(mir)?;
        let built_from = self.cur;
        self.jump(join)?;
        self.switch(loaded);
        self.jump(join)?;
        self.switch(join);
        let table = self.b.ins_stack_phi(vec![(built_from, built), (loaded, have)]).map_err(mir)?;
        self.b.mark_invariant_load(table);
        Ok(Val::V(table))
    }

    /// `new C(..)`: every argument, then the object and each field's store
    /// (no argument sees the object, so none runs with it allocated).
    fn make_object(&mut self, id: HirId, args: &[HirId]) -> Result<Val, Refusal> {
        let type_id = self.news(id).expect("new object").0;
        let Some(words) = self.object_words(id, args)? else {
            return Ok(Val::Never);
        };
        let nfields = words.len() as u32;
        let obj = self.b.ins_alloc(MirAllocKind::Object { type_id, nfields }, Vec::new()).map_err(mir)?;
        let obj = self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![obj]).map_err(mir)?;
        for (slot, v) in words.into_iter().enumerate() {
            self.b.ins_heap_field_store(obj, v, slot as u32).map_err(mir)?;
        }
        Ok(Val::V(obj))
    }

    /// The field words of `new C(args)` (`id`), each argument run in
    /// order into the word its slot holds; `None` if one never returns.
    fn object_words(&mut self, id: HirId, args: &[HirId]) -> Result<Option<Vec<ValueId>>, Refusal> {
        let tys = self.news(id).expect("new object").1.clone();
        let tails = self.new_fact(id).tails.clone();
        let mut words = Vec::with_capacity(args.len());
        let mut tail_words = Vec::new();
        for (i, (&arg, ty)) in args.iter().zip(tys).enumerate() {
            // A flat two-word field: its first word here, its second in
            // its tail slot.
            if let Some(&Some(tail)) = tails.get(i) {
                let Some((lo, hi)) = self.pair(arg)? else {
                    return Ok(None);
                };
                words.push(lo);
                tail_words.push((tail, hi));
                continue;
            }
            let v = match self.boxed_niche(arg) {
                Some(layout) => self.boxed_niche_word(arg, layout)?,
                None if ty == MirTy::HeapRef => self.boxed_word(arg)?,
                None => self.word(arg)?,
            };
            let Some(v) = v else {
                return Ok(None);
            };
            if self.b.value_ty(v) != ty {
                return Err("field type".into());
            }
            words.push(v);
        }
        tail_words.sort_by_key(|&(tail, _)| tail);
        for (tail, hi) in tail_words {
            if tail as usize != words.len() {
                return Err("tail slot".into());
            }
            words.push(hi);
        }
        Ok(Some(words))
    }

    /// A boxed user enum variant: `MakeEnum` of the payload words.
    fn make_boxed(&mut self, enum_name: &str, tag: u32, tys: &[Ty], args: &[HirId]) -> Result<Val, Refusal> {
        // A unit variant is a shared immortal: only payload variants drop.
        let drop_type = if args.is_empty() { None } else { self.cx.enum_drop(enum_name) };
        let mut words = Vec::with_capacity(args.len());
        for (&arg, t) in args.iter().zip(tys) {
            // A `()` payload is the empty tuple object, after a `()`-typed
            // value's effect (`Some(f())`, #1449).
            if query::is_unit_make(self.hir, arg) || is_unit(t) {
                if !self.effect(arg)? {
                    return Ok(Val::Never);
                }
                words.push(self.unit_object()?);
                continue;
            }
            let want = self.word_ty(t)
                .filter(|_| !is_unit(t))
                .or_else(|| self.cx.boxed_view(t, None).map(|_| MirTy::HeapRef))
                // An open tuple (`(T, int)` in a shared body): its heap
                // tuple, as the ground one is stored.
                .or_else(|| is_heap_tuple(t).then_some(MirTy::HeapRef))
                .ok_or("make payload type")?;
            // A two-word payload is stored boxed.
            let v = if want == MirTy::HeapRef { self.boxed_word(arg)? } else { self.word(arg)? };
            let Some(v) = v else {
                return Ok(Val::Never);
            };
            if self.b.value_ty(v) != want {
                return Err("make payload word".into());
            }
            words.push(v);
        }
        // Declaration-order payload kinds, as HIR emission's `MakeEnumK`; a
        // `()` payload is the empty tuple object, a pointer.
        let kinds = common::pack_word_kinds(args.iter().zip(tys).map(|(&a, t)| {
            if query::is_unit_make(self.hir, a) || is_unit(t) {
                common::WORD_POINTER
            } else {
                self.cx.word_kind(t)
            }
        }));
        let kind = match drop_type {
            Some(type_id) => MirAllocKind::DropEnum { tag, type_id },
            None => MirAllocKind::Enum { tag },
        };
        let obj = self.b.ins_alloc_kinds(kind, words, kinds).map_err(mir)?;
        Ok(Val::V(self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![obj]).map_err(mir)?))
    }

    /// A two-word enum value boxed to one word: the tag tested against each variant's but
    /// the last, then that variant made from the payload word.
    fn box_pair(&mut self, ty: &Ty, lo: ValueId, tag: ValueId) -> Result<Val, Refusal> {
        let (name, variants) = self.cx.pair_variants(ty).ok_or("box pair type")?;
        if self.cx.enum_drop(&name).is_some() {
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
        let ty = self.expr_ty(recv).and_then(|t| self.elem_word(t));
        if !self.heap_array(recv) {
            return Err("push receiver".into());
        }
        let Some(arr) = self.word(recv)? else {
            return Ok(Val::Never);
        };
        if let Some(lane) = self.lane(recv) {
            let Some((lo, hi)) = self.pair(value)? else {
                return Ok(Val::Never);
            };
            let out = self.b.ins_lane_push(arr, lo, hi, lane.shape).map_err(mir)?;
            let out = self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![out]).map_err(mir)?;
            return self.pushed(recv, out);
        }
        let v = if ty == Some(MirTy::HeapRef) { self.boxed_word(value)? } else { self.word(value)? };
        let Some(v) = v else {
            return Ok(Val::Never);
        };
        let v = match ty {
            Some(MirTy::NicheOpt) if self.b.value_ty(v) == MirTy::NicheRes => self.fit(v, MirTy::NicheOpt)?,
            _ => v,
        };
        if Some(self.b.value_ty(v)) != ty {
            return Err("push value type".into());
        }
        let out = self.b.ins_array_push(arr, v).map_err(mir)?;
        let out = self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![out]).map_err(mir)?;
        self.pushed(recv, out)
    }

    /// After a push onto `recv`: the pushed array, as the safepoint gives
    /// it, is the local's value from here on: a loop's φ for it then takes
    /// this word on the latch rather than merging only with itself (#1295).
    fn pushed(&mut self, recv: HirId, out: ValueId) -> Result<Val, Refusal> {
        if let HirKind::Local(local) = self.hir.expr(recv).kind
            && self.local_ty(local).ok() == Some(MirTy::HeapRef)
        {
            let slot = self.slot(local)?;
            self.keep_pin(local)?;
            self.b.def_local(slot, out).map_err(mir)?;
        }
        Ok(Val::Unit)
    }

    /// `format(fmt, args)` with word arguments: `FORMAT` of the format string
    /// and each argument, a heap string the collector may see at once.
    fn format(&mut self, id: HirId, fmt: u32, args: &[HirId]) -> Result<Val, Refusal> {
        // The string first, as emission loads it, so the arguments land in
        // the registers after it.
        let fmt = self.b.ins_string(fmt).map_err(mir)?;
        let shows = self.site(id).format_shows.as_ref();
        let mut words = Vec::with_capacity(args.len());
        for (i, &arg) in args.iter().enumerate() {
            // A two-word argument goes boxed, as emission passes it.
            let Some(v) = self.word_or_boxed(arg)? else {
                return Ok(Val::Never);
            };
            // `%v`: the value's text.
            let v = match shows.and_then(|s| s.get(i)).and_then(Option::as_ref) {
                Some(show) => self.show(id, v, show)?,
                None => v,
            };
            words.push(v);
        }
        let out = self.b.ins_format(fmt, words).map_err(mir)?;
        Ok(Val::V(self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![out]).map_err(mir)?))
    }

    /// The text of `v` for a `%v` argument of `format` call `id`: a raw `show` call, `STRINGIFY`, or a
    /// tuple's or record's items shown and formatted.
    fn show(&mut self, id: HirId, v: ValueId, show: &HirShow) -> Result<ValueId, Refusal> {
        let (fmt, parts, tuple) = match show {
            HirShow::Call { target, word, dicts } => {
                // Only a heap carrier goes as a word here, as an operator's
                // operand.
                let v = match *word {
                    false => v,
                    true if self.b.value_ty(v) != MirTy::HeapRef => return Err("show word".into()),
                    true => self.b.ins_word(v).map_err(mir)?,
                };
                let mut words = vec![v];
                words.extend(self.dict_values(dicts)?);
                let abi = CallAbi {
                    params: words.iter().map(|&w| self.b.value_ty(w)).collect(),
                    ret: MirTy::HeapRef,
                    ret_hi: None,
                };
                return Ok(self.at_expr(id, |l| l.b.ins_call(*target, words, &abi).map_err(mir))?.0);
            }
            HirShow::Stringify => {
                let s = self.at_expr(id, |l| l.b.ins_stringify(v, None).map_err(mir))?;
                return self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![s]).map_err(mir);
            }
            &HirShow::Bound { dict, method } => {
                let dict = *self.dict_params.get(&dict).ok_or("dictionary parameter")?;
                let callee = self.b.ins_tuple_item(dict, method, MirTy::I64).map_err(mir)?;
                return self.at_expr(id, |l| l.b.ins_call_indirect(callee, vec![v, dict], MirTy::HeapRef).map_err(mir));
            }
            HirShow::Product { fmt, parts, tuple } => (fmt, parts, *tuple),
        };
        if self.b.value_ty(v) != MirTy::HeapRef {
            return Err("show aggregate".into());
        }
        let mut words = Vec::with_capacity(parts.len());
        for (slot, (ty, part)) in parts.iter().enumerate() {
            // An enum item is held as its boxed object, the word its
            // `show` takes (#1176); a niche `Option` / `Result` item is its
            // niche word (`tuple_item`), which its raw `show` takes as it
            // is (#1611).
            let ty_w = self
                .tuple_item(ty)
                .or_else(|| self.cx.boxed_view(ty, None).is_some().then_some(MirTy::HeapRef))
                .ok_or("show item type")?;
            let slot = slot as u32;
            let w = self.at_expr(id, |l| if tuple { l.b.ins_tuple_item(v, slot, ty_w) } else { l.b.ins_product_load(v, slot, ty_w) }.map_err(mir))?;
            words.push(self.show(id, w, part)?);
        }
        let fmt = self.b.ins_string(*fmt).map_err(mir)?;
        let out = self.b.ins_format(fmt, words).map_err(mir)?;
        self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![out]).map_err(mir)
    }

    /// A matrix / vector operator or `dot` / `matmul` on its packed
    /// kernel: each operand's
    /// word, then the meta word, then `HostInvoke`. The result is the
    /// expression's word: a new matrix or vector array, or `dot`'s scalar.
    fn linalg(&mut self, id: HirId, (native, meta): (u16, u32), args: &[HirId]) -> Result<Val, Refusal> {
        // A tuple result (a tuple of row tuples) is one boxed tuple word.
        let ret = self
            .expr_ty(id)
            .and_then(|t| self.word_ty(t).or_else(|| self.plain_heap(t).then_some(MirTy::HeapRef)))
            .filter(|_| self.pair_tys(id).is_none())
            .ok_or("linear algebra result")?;
        let mut words = Vec::with_capacity(args.len() + 1);
        for &arg in args {
            let Some(v) = self.word_or_boxed(arg)? else {
                return Ok(Val::Never);
            };
            words.push(v);
        }
        words.push(self.b.ins_const(MirConst::I64(i64::from(meta))).map_err(mir)?);
        let out = self.at_expr(id, |l| {
            l.b.ins_host_invoke_as(native, words, common::HOST_ENUM_LAYOUT_BOXED as u8, Some(ret)).map_err(mir)
        })?;
        Ok(Val::V(out))
    }

    /// `cross(a, b)` of two 3-vectors, unrolled: each component read, then
    /// `(ay*bz - az*by, az*bx - ax*bz, ax*by - ay*bx)` as a tuple or array.
    fn cross(&mut self, id: HirId, (tuple, float, byte): (bool, bool, bool), args: &[HirId]) -> Result<Val, Refusal> {
        let [a, b] = *args else {
            return Err("cross arity".into());
        };
        let elem = if float { MirTy::F64 } else { MirTy::I64 };
        let mut parts = Vec::with_capacity(6);
        let mut objs = Vec::with_capacity(2);
        for arg in [a, b] {
            let Some(obj) = self.word_or_boxed(arg)? else {
                return Ok(Val::Never);
            };
            if self.b.value_ty(obj) != MirTy::HeapRef {
                return Err("cross operand".into());
            }
            objs.push(obj);
        }
        for obj in objs {
            for i in 0..3 {
                let k = self.b.ins_const(MirConst::I64(i)).map_err(mir)?;
                parts.push(self.b.ins_index(obj, k, elem, false).map_err(mir)?);
            }
        }
        let [ax, ay, az, bx, by, bz] = parts[..] else {
            unreachable!("six components")
        };
        let mut words = Vec::with_capacity(3);
        for (p, q, r, s) in [(ay, bz, az, by), (az, bx, ax, bz), (ax, by, ay, bx)] {
            // An int product or difference traps on overflow at the call.
            let w = self.at_expr(id, |l2| {
                let l = l2.b.ins_binop(MirBinOp::Mul, p, q).map_err(mir)?;
                let r = l2.b.ins_binop(MirBinOp::Mul, r, s).map_err(mir)?;
                l2.b.ins_binop(MirBinOp::Sub, l, r).map_err(mir)
            })?;
            words.push(self.mask_byte(w, byte)?);
        }
        let ty = self.expr_ty(id).ok_or("cross type")?.clone();
        let obj = if tuple {
            let kinds = common::pack_word_kinds(std::iter::repeat_n(common::WORD_SCALAR, 3));
            self.b.ins_alloc_kinds(MirAllocKind::TUPLE, words, kinds).map_err(mir)?
        } else {
            let stamp = array_elem(&ty).map_or(0, |e| self.cx.array_stamp(e));
            self.b.ins_alloc_kinds(MirAllocKind::Array, words, stamp).map_err(mir)?
        };
        Ok(Val::V(self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![obj]).map_err(mir)?))
    }

    /// A linear-algebra op past the packed kernel's limits, unrolled:
    /// `dot`'s products
    /// summed in order, and each cell of a matrix product, element-wise
    /// operator or negation; a matrix's rows and the matrix are made as
    /// tuples or arrays with no word kinds, as `MakeTuple` / `MakeArray`
    /// there.
    fn unrolled_la(&mut self, id: HirId, kind: crate::typechecking::LinearAlgebraKind, args: &[HirId]) -> Result<Val, Refusal> {
        use crate::typechecking::{LinearAlgebraKind as K, ScalarSide};
        let scalar = match &kind {
            K::MatrixZip { scalar_on, .. } => *scalar_on,
            _ => None,
        };
        let mut objs = Vec::with_capacity(args.len());
        for (i, &arg) in args.iter().enumerate() {
            let Some(obj) = self.word_or_boxed(arg)? else {
                return Ok(Val::Never);
            };
            let is_scalar = matches!((scalar, i), (Some(ScalarSide::Left), 0) | (Some(ScalarSide::Right), 1));
            if !is_scalar && self.b.value_ty(obj) != MirTy::HeapRef {
                return Err("linear algebra operand".into());
            }
            objs.push(obj);
        }
        let elem_of = |float: bool| if float { MirTy::F64 } else { MirTy::I64 };
        let at = |l: &mut Self, obj: ValueId, i: usize, ty: MirTy| -> Result<ValueId, Refusal> {
            let i = l.b.ins_const(MirConst::I64(i as i64)).map_err(mir)?;
            l.b.ins_index(obj, i, ty, false).map_err(mir)
        };
        // `m[i][j]`, each row read again as emission's `LOAD; CONST; INDEX`.
        let cell = |l: &mut Self, obj: ValueId, i: usize, j: usize, ty: MirTy| -> Result<ValueId, Refusal> {
            let row = at(l, obj, i, MirTy::HeapRef)?;
            at(l, row, j, ty)
        };
        let bin = |l: &mut Self, op: MirBinOp, a: ValueId, b: ValueId| l.at_expr(id, |l2| l2.b.ins_binop(op, a, b).map_err(mir));
        let made = |l: &mut Self, tuple: bool, words: Vec<ValueId>| -> Result<ValueId, Refusal> {
            let kind = if tuple { MirAllocKind::TUPLE } else { MirAllocKind::Array };
            let obj = l.b.ins_alloc(kind, words).map_err(mir)?;
            l.b.ins_gc_barrier(MirGcKind::Safepoint, vec![obj]).map_err(mir)
        };
        // `m` rows of `n` cells, each from `f(i, j)`.
        let matrix = |l: &mut Self, (m, n): (usize, usize), (outer, row): (bool, bool), f: &mut dyn FnMut(&mut Self, usize, usize) -> Result<ValueId, Refusal>| -> Result<Val, Refusal> {
            let mut rows = Vec::with_capacity(m);
            for i in 0..m {
                let mut cells = Vec::with_capacity(n);
                for j in 0..n {
                    cells.push(f(l, i, j)?);
                }
                rows.push(made(l, row, cells)?);
            }
            Ok(Val::V(made(l, outer, rows)?))
        };
        match kind {
            K::Dot { length, elem_is_float, elem_is_byte, .. } if length > 0 && objs.len() == 2 => {
                let elem = elem_of(elem_is_float);
                let mut acc = None;
                for i in 0..length {
                    let x = at(self, objs[0], i, elem)?;
                    let y = at(self, objs[1], i, elem)?;
                    let p = bin(self, MirBinOp::Mul, x, y)?;
                    acc = Some(match acc {
                        None => p,
                        Some(acc) => bin(self, MirBinOp::Add, acc, p)?,
                    });
                }
                let sum = acc.ok_or("empty dot")?;
                Ok(Val::V(self.mask_byte(sum, elem_is_byte)?))
            }
            K::MatMul { m, k, n, outer_is_tuple, row_is_tuple, elem_is_float, elem_is_byte } if k > 0 && objs.len() == 2 => {
                let elem = elem_of(elem_is_float);
                let (a, b) = (objs[0], objs[1]);
                matrix(self, (m, n), (outer_is_tuple, row_is_tuple), &mut |l, i, j| {
                    let mut acc = None;
                    for t in 0..k {
                        let x = cell(l, a, i, t, elem)?;
                        let y = cell(l, b, t, j, elem)?;
                        let p = bin(l, MirBinOp::Mul, x, y)?;
                        acc = Some(match acc {
                            None => p,
                            Some(acc) => bin(l, MirBinOp::Add, acc, p)?,
                        });
                    }
                    let sum = acc.ok_or("empty matmul")?;
                    l.mask_byte(sum, elem_is_byte)
                })
            }
            K::MatrixZip { m, n, op, outer_is_tuple, row_is_tuple, elem_is_float, elem_is_byte, scalar_on } if objs.len() == 2 => {
                let elem = elem_of(elem_is_float);
                let (a, b) = (objs[0], objs[1]);
                let (left, right) = (scalar_on == Some(ScalarSide::Left), scalar_on == Some(ScalarSide::Right));
                matrix(self, (m, n), (outer_is_tuple, row_is_tuple), &mut |l, i, j| {
                    let x = if left { a } else { cell(l, a, i, j, elem)? };
                    let y = if right { b } else { cell(l, b, i, j, elem)? };
                    l.matrix_cell(id, op, elem_is_float, elem_is_byte, x, y)
                })
            }
            K::MatrixNeg { m, n, outer_is_tuple, row_is_tuple, elem_is_float, elem_is_byte, bit_not } if objs.len() == 1 => {
                let elem = elem_of(elem_is_float);
                let a = objs[0];
                matrix(self, (m, n), (outer_is_tuple, row_is_tuple), &mut |l, i, j| {
                    let x = cell(l, a, i, j, elem)?;
                    if !bit_not {
                        let v = l.at_expr(id, |l2| l2.b.ins_neg(x).map_err(mir))?;
                        return l.mask_byte(v, elem_is_byte);
                    }
                    let ones = l.b.ins_const(MirConst::I64(-1)).map_err(mir)?;
                    let v = l.b.ins_binop(MirBinOp::Xor, x, ones).map_err(mir)?;
                    l.mask_byte(v, elem_is_byte)
                })
            }
            _ => Err("linear algebra unrolled".into()),
        }
    }

    /// `v & 255` for a `byte` cell or element: the scalar operators' wrap.
    fn mask_byte(&mut self, v: ValueId, byte: bool) -> Result<ValueId, Refusal> {
        if !byte {
            return Ok(v);
        }
        let mask = self.b.ins_const(MirConst::I64(255)).map_err(mir)?;
        self.b.ins_binop(MirBinOp::BitAnd, v, mask).map_err(mir)
    }

    /// `byte` `base ** exp` modulo 256 with no int overflow: square and
    /// multiply over the 8 bits of `exp`, each step masked, as the scalar
    /// operator (`build::byte_pow`).
    fn byte_pow(&mut self, base: ValueId, exp: ValueId) -> Result<ValueId, Refusal> {
        let int = |l: &mut Self, n: i64| l.b.ins_const(MirConst::I64(n)).map_err(mir);
        let bin = |l: &mut Self, op: MirBinOp, a: ValueId, b: ValueId| l.b.ins_binop(op, a, b).map_err(mir);
        let one = int(self, 1)?;
        let (mut sq, mut acc) = (base, one);
        for i in 0..8 {
            // `sq` when bit `i` of `exp` is set, else 1.
            let k = int(self, i)?;
            let shifted = bin(self, MirBinOp::Shr, exp, k)?;
            let bit = bin(self, MirBinOp::BitAnd, shifted, one)?;
            let taken = bin(self, MirBinOp::Mul, bit, sq)?;
            let skipped = bin(self, MirBinOp::Xor, bit, one)?;
            let factor = bin(self, MirBinOp::Add, taken, skipped)?;
            let product = bin(self, MirBinOp::Mul, acc, factor)?;
            acc = self.mask_byte(product, true)?;
            if i < 7 {
                let square = bin(self, MirBinOp::Mul, sq, sq)?;
                sq = self.mask_byte(square, true)?;
            }
        }
        Ok(acc)
    }

    /// One cell of an element-wise matrix operator.
    fn matrix_cell(&mut self, id: HirId, op: crate::typechecking::MatrixCellOp, float: bool, byte: bool, a: ValueId, b: ValueId) -> Result<ValueId, Refusal> {
        use crate::typechecking::MatrixCellOp as C;
        // Emission tests a float cell equal as `a <= b && a >= b`: IEEE
        // `==` (false at a NaN, `-0.0 == 0.0`), each operand read once;
        // not equal (or nonzero) is its negation, true at a NaN.
        let nonzero = |l: &mut Self, v: ValueId| -> Result<ValueId, Refusal> {
            if float {
                let zero = l.b.ins_const(MirConst::F64(0.0f64.to_bits())).map_err(mir)?;
                let eq = l.b.ins_cmp(MirCmpOp::Eq, v, zero).map_err(mir)?;
                l.b.ins_not(eq).map_err(mir)
            } else {
                let zero = l.b.ins_const(MirConst::I64(0)).map_err(mir)?;
                l.b.ins_cmp(MirCmpOp::Ne, v, zero).map_err(mir)
            }
        };
        let cmp = |op: C| match op {
            C::Eq => Some(MirCmpOp::Eq),
            C::Ne => Some(MirCmpOp::Ne),
            C::Lt => Some(MirCmpOp::Lt),
            C::Le => Some(MirCmpOp::Le),
            C::Gt => Some(MirCmpOp::Gt),
            C::Ge => Some(MirCmpOp::Ge),
            _ => None,
        };
        match op {
            C::Add | C::Sub => {
                let op = if op == C::Add { MirBinOp::Add } else { MirBinOp::Sub };
                let v = self.at_expr(id, |l| l.b.ins_binop(op, a, b).map_err(mir))?;
                self.mask_byte(v, byte)
            }
            C::Ne if float => {
                let eq = self.b.ins_cmp(MirCmpOp::Eq, a, b).map_err(mir)?;
                self.b.ins_not(eq).map_err(mir)
            }
            C::Eq | C::Ne | C::Lt | C::Le | C::Gt | C::Ge => {
                let op = cmp(op).ok_or("matrix cell compare")?;
                self.b.ins_cmp(op, a, b).map_err(mir)
            }
            C::BitAnd | C::BitOr | C::BitXor => {
                let op = match op {
                    C::BitAnd => MirBinOp::BitAnd,
                    C::BitOr => MirBinOp::BitOr,
                    _ => MirBinOp::Xor,
                };
                let v = self.b.ins_binop(op, a, b).map_err(mir)?;
                self.mask_byte(v, byte)
            }
            C::Shl | C::Shr => {
                let mask = self.b.ins_const(MirConst::I64(63)).map_err(mir)?;
                let count = self.b.ins_binop(MirBinOp::BitAnd, b, mask).map_err(mir)?;
                let op = if op == C::Shl { MirBinOp::Shl } else { MirBinOp::Shr };
                let v = self.at_expr(id, |l| l.b.ins_binop(op, a, count).map_err(mir))?;
                self.mask_byte(v, byte)
            }
            C::Intersect | C::Diff => {
                let x = nonzero(self, a)?;
                let y = if op == C::Intersect {
                    nonzero(self, b)?
                } else {
                    let y = nonzero(self, b)?;
                    self.b.ins_not(y).map_err(mir)?
                };
                self.b.ins_bool_logic(MirBinOp::BitAnd, x, y).map_err(mir)
            }
        }
    }

    /// A host native: the arguments in order, then `HostInvoke` (the native
    /// id is the instruction's, not a value the lowering has to find).
    fn host(&mut self, id: HirId, args: &[HirId]) -> Result<Val, Refusal> {
        // A `Vec` native a thunk inlined, on a lane array.
        if let Some(lane_call) = self.lane_call_of(id)
            && lane_call != HirLaneCall::Thunk
        {
            return self.lane_call(id, args, lane_call);
        }
        let (mut native, layout, unbox) = self.host_call(id).cloned().expect("host call");
        // A `from_bytes` whose buffer is dead after it takes the bytes.
        if self.hir.expr(id).flags.contains(crate::hir::HirFlags::TAKE) && common::host_native_id("from_bytes") == Some(usize::from(native)) {
            native = common::FROM_BYTES_TAKE_ID;
        }
        let raw = std::mem::take(&mut self.raw_host);
        let pair = match (self.pair_tys(id), unbox) {
            (Some(_), Some(_)) if raw => None,
            (Some(tys), Some(variants)) => Some((tys, variants)),
            (Some(_), None) => return Err("host pair result".into()),
            (None, _) => None,
        };
        // A tuple (`io::capture_end`'s two strings) comes back as its boxed
        // tuple object.
        let tuple = pair.is_none() && !raw && self.expr_ty(id).is_some_and(|t| matches!(strip_readonly(t), Ty::Tuple(items) if !items.is_empty()));
        let unit = self.expr_ty(id).is_some_and(is_unit);
        // A two-word `Option` / `Result` comes back as its two words
        // (`HOST_ENUM_LAYOUT_PAIR`); a raw read and a tuple stay boxed.
        // A payload word of another type (`Option<bool>`) comes boxed.
        let two_words = pair.is_some() && self.host_two_words(id);
        if self.pair_tys(id).is_some() && !two_words {
            let name = common::HOST_NATIVES.iter().find(|h| h.id == native).map_or("?", |h| h.name);
            let ty = self.expr_ty(id).map(|t| crate::typechecking::pretty::format_ty_for_diag(&Default::default(), t)).unwrap_or_default();
            super::box_report::note(&self.b.func().name, super::box_report::BoxKind::HostResult, || format!("{name} -> {ty}"));
        }
        let ret = match self.expr_ty(id) {
            _ if pair.is_some() || tuple || raw => Some(MirTy::HeapRef),
            _ if unit => None,
            // A boxed enum (`thread_spawn`'s `Result<thread, ThreadError>`)
            // stays the object it comes back as.
            Some(ty) => Some(
                self.word_ty(ty)
                    .or_else(|| self.cx.boxed_view(ty, None).map(|_| MirTy::HeapRef))
                    .ok_or_else(|| format!("host result {}", ty_kind(ty)))?,
            ),
            None => return Err("host result".into()),
        };
        let mut words = Vec::with_capacity(args.len());
        for &arg in args {
            // A two-word value (`Option<int>`, a pair tuple) goes boxed, as
            // to a plain call's boxed parameter (#1414).
            match self.value(arg)? {
                Val::V(v) => words.push(v),
                Val::Never => return Ok(Val::Never),
                // `weak(())`: the empty tuple object.
                Val::Unit => words.push(self.unit_object()?),
                Val::Pair(lo, hi) => {
                    let ty = self.expr_ty(arg).ok_or("host argument type")?.clone();
                    match self.box_pair_val(&ty, lo, hi)? {
                        Val::V(v) => words.push(v),
                        Val::Never => return Ok(Val::Never),
                        _ => return Err("host argument boxed".into()),
                    }
                }
            }
        }
        if let Some(((lo_ty, _), _)) = pair
            && two_words
        {
            let (lo, hi) = self.at_expr(id, |l| l.b.ins_host_invoke_pair(native, words, lo_ty).map_err(mir))?;
            if self.heap_pair(id) {
                self.b.mark_heap_pair_word(lo);
            }
            return Ok(Val::Pair(lo, hi));
        }
        let out = self.at_expr(id, |l| l.b.ins_host_invoke_as(native, words, layout, ret).map_err(mir))?;
        if let Some(((lo_ty, _), variants)) = pair {
            return self.unbox_pair(out, lo_ty, &variants);
        }
        if tuple && let Some((lo_ty, hi_ty)) = self.pair_tys(id) {
            let ty = self.expr_ty(id).ok_or("host tuple type")?.clone();
            return self.unbox_word(&ty, out, lo_ty, hi_ty);
        }
        Ok(if unit { Val::Unit } else { Val::V(out) })
    }

    /// An FFI opcode over its operands, then its boxed `Result`: an
    /// `extern`'s `Ok` payload (an `Err` panics with the error's message),
    /// the niche word it converts to, or
    /// the object as it comes back.
    fn ffi(&mut self, id: HirId, site: &HirFfi) -> Result<Val, Refusal> {
        let ret = match self.expr_ty(id) {
            _ if !site.unwrap => None,
            Some(ty) if is_unit(ty) => None,
            Some(ty) => Some(self.word_ty(ty).ok_or_else(|| format!("ffi result {}", ty_kind(ty)))?),
            None => return Err("ffi result".into()),
        };
        let mut words = Vec::with_capacity(site.operands.len());
        for op in &site.operands {
            let Some(w) = self.ffi_operand(op)? else {
                return Ok(Val::Never);
            };
            words.push(w);
        }
        let obj = self.at_expr(id, |l| l.b.ins_ffi(site.op, site.operand, words).map_err(mir))?;
        if !site.unwrap {
            return match site.niche {
                Some(want) => self.unbox_niche(obj, want),
                None => Ok(Val::V(obj)),
            };
        }
        let (ok, err) = (self.b.create_block(), self.b.create_block());
        // `Ok` always carries one word, a void call's too (the runtime's
        // `Ok(0)`): the match takes it, and the taken edge passes it as a
        // block argument (#1638).
        let word = self.b.ins_match_payload(obj, 0, ret.unwrap_or(MirTy::I64)).map_err(mir)?;
        let payload = ret.map(|_| word);
        self.b.jump_if_match(obj, 0, vec![word], ok, err).map_err(mir)?;
        let start = self.order.len();
        self.switch(err);
        self.at_expr(id, |l| {
            let e = l.b.ins_match_payload(obj, 0, MirTy::HeapRef).map_err(mir)?;
            let msg = l.b.ins_product_load(e, 1, MirTy::HeapRef).map_err(mir)?;
            l.keep_pins()?;
            l.b.panic(msg).map_err(mir)
        })?;
        self.mark_cold(start);
        self.switch(ok);
        Ok(match payload {
            Some(p) => Val::V(p),
            None => Val::Unit,
        })
    }

    /// One FFI operand's word (`None` when evaluating it never returns).
    fn ffi_operand(&mut self, op: &HirFfiOperand) -> Result<Option<ValueId>, Refusal> {
        Ok(Some(match op {
            HirFfiOperand::Static(slot) => self.b.ins_load_static(*slot, MirTy::I64).map_err(mir)?,
            HirFfiOperand::Value(id) => match self.word(HirId(*id))? {
                Some(v) => v,
                None => return Ok(None),
            },
            HirFfiOperand::Const(c) => self.b.ins_const(MirConst::I64(*c)).map_err(mir)?,
            HirFfiOperand::CodePtr(target) => self.b.ins_code_ptr(*target).map_err(mir)?,
            HirFfiOperand::Tuple(items) => {
                let mut words = Vec::with_capacity(items.len());
                for item in items {
                    let Some(w) = self.ffi_operand(item)? else {
                        return Ok(None);
                    };
                    words.push(w);
                }
                let t = self.b.ins_alloc(MirAllocKind::TUPLE, words).map_err(mir)?;
                self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![t]).map_err(mir)?
            }
        }))
    }

    /// An FFI call whose `Result` is kept as the object it comes back as.
    fn ffi_boxed(&self, id: HirId) -> bool {
        self.site(id).ffi.as_ref().is_some_and(|s| !s.unwrap && s.niche.is_none())
    }

    /// A boxed enum word to `[payload, tag]`: a `JumpIfMatch` per variant
    /// but the last,
    /// which the fallthrough unpacks.
    fn unbox_pair(&mut self, obj: ValueId, lo_ty: MirTy, variants: &[(u32, u32)]) -> Result<Val, Refusal> {
        let Some((&(last_tag, last_arity), rest)) = variants.split_last() else {
            return Err("host pair variants".into());
        };
        let mut out = Vec::with_capacity(variants.len());
        for &(t, arity) in rest {
            let word = match arity {
                0 => None,
                1 | UNIT_PAYLOAD => Some(self.b.ins_match_payload(obj, 0, lo_ty).map_err(mir)?),
                _ => return Err("host pair arity".into()),
            };
            let (hit, miss) = (self.b.create_block(), self.b.create_block());
            self.b.jump_if_match(obj, t, word.into_iter().collect(), hit, miss).map_err(mir)?;
            self.switch(hit);
            let p = match word {
                Some(w) if arity == 1 => w,
                _ => self.zero(lo_ty)?,
            };
            let tag = self.b.ins_const(MirConst::I64(i64::from(t))).map_err(mir)?;
            out.push((self.cur, Val::Pair(p, tag)));
            self.switch(miss);
        }
        let p = match last_arity {
            0 | UNIT_PAYLOAD => self.zero(lo_ty)?,
            1 => self.b.ins_match_payload(obj, 0, lo_ty).map_err(mir)?,
            _ => return Err("host pair arity".into()),
        };
        let tag = self.b.ins_const(MirConst::I64(i64::from(last_tag))).map_err(mir)?;
        out.push((self.cur, Val::Pair(p, tag)));
        self.join_arms(out)
    }

    /// A boxed `Option` / `Result` word to its niche word, as HIR
    /// emission's boxed-to-niche conversion: `None` is 0 and `Some(p)` is
    /// `p`; `Ok(p)` is `p` and `Err(p)` is `p | 1`.
    fn unbox_niche(&mut self, obj: ValueId, want: MirTy) -> Result<Val, Refusal> {
        let (hit, miss) = (self.b.create_block(), self.b.create_block());
        let p = self.b.ins_match_payload(obj, 0, MirTy::HeapRef).map_err(mir)?;
        let mut out = Vec::with_capacity(2);
        match want {
            MirTy::NicheOpt => {
                self.b.jump_if_match(obj, 1, vec![p], hit, miss).map_err(mir)?;
                self.switch(hit);
                let w = self.b.ins_cast(MirCastKind::Word, want, p).map_err(mir)?;
                out.push((self.cur, Val::V(w)));
                self.switch(miss);
                let zero = self.b.ins_const(MirConst::I64(0)).map_err(mir)?;
                let w = self.b.ins_cast(MirCastKind::Word, want, zero).map_err(mir)?;
                out.push((self.cur, Val::V(w)));
            }
            MirTy::NicheRes => {
                self.b.jump_if_match(obj, 1, vec![p], hit, miss).map_err(mir)?;
                self.switch(hit);
                let word = self.b.ins_cast(MirCastKind::Word, MirTy::I64, p).map_err(mir)?;
                let one = self.b.ins_const(MirConst::I64(1)).map_err(mir)?;
                let tagged = self.b.ins_binop(MirBinOp::BitOr, word, one).map_err(mir)?;
                let w = self.b.ins_cast(MirCastKind::Word, want, tagged).map_err(mir)?;
                out.push((self.cur, Val::V(w)));
                self.switch(miss);
                let ok = self.b.ins_match_payload(obj, 0, MirTy::HeapRef).map_err(mir)?;
                let w = self.b.ins_cast(MirCastKind::Word, want, ok).map_err(mir)?;
                out.push((self.cur, Val::V(w)));
            }
            _ => return Err("boxed call result".into()),
        }
        self.join_arms(out)
    }

    /// `x.len()`: a fixed size (a local or literal is not read; any other
    /// operand is evaluated and dropped), or the heap array's length.
    fn len(&mut self, arg: HirId, fixed: Option<u32>) -> Result<Val, Refusal> {
        if let Some(n) = fixed {
            if !matches!(self.hir.expr(arg).kind, HirKind::Local(_) | HirKind::Lit(_) | HirKind::Make { .. }) && !self.effect(arg)? {
                return Ok(Val::Never);
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
    /// `continue`). A tuple loops over an array of its items. A tuple
    /// pattern binds each part from the item.
    fn for_array(&mut self, id: HirId, pat: &HirPat, iterable: HirId, body: HirId, into_iter: Option<Label>, items: Items) -> Result<Val, Refusal> {
        if into_iter.is_none()
            && matches!(items, Items::Array)
            && let Some(lane) = self.lane(iterable)
        {
            return self.for_lanes(pat, iterable, body, lane);
        }
        let it_ty = self.expr_ty(iterable).cloned();
        // A user `into_iter`'s array holds the loop's items.
        let elem = match (into_iter, items, &it_ty) {
            (Some(_), _, _) => match pat {
                HirPat::Bind(local) => scalar(self.hir.local(*local).ty.as_ref().ok_or("local type")?),
                _ => None,
            },
            (None, Items::Array, Some(t)) => self.elem_word(t),
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
        // A tuple pattern nesting another (`for (a, (b, c)) in xs`) of an
        // array's tuple items: read out as `let` reads one (#2783).
        let nested_ty = match pat {
            HirPat::Tuple(pats) if elem == MirTy::HeapRef && into_iter.is_none() && pats.iter().any(|p| matches!(p, HirPat::Tuple(_))) => {
                Some(it_ty.as_ref().and_then(|t| array_elem(t)).cloned().ok_or("for in pattern type")?)
            }
            _ => None,
        };
        // Where the item goes: the bound local, or a temp the pattern's
        // parts read.
        let parts: Option<Vec<(HirLocal, MirTy)>> = match pat {
            _ if nested_ty.is_some() => None,
            // A `()` local has no slot: its item is read and dropped.
            HirPat::Bind(local) if self.unit_local(*local) => None,
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
                        // A `()` local has no slot (#1427).
                        HirPat::Wild => {}
                        HirPat::Bind(l) if self.unit_local(*l) => {}
                        HirPat::Bind(l) => {
                            let ty = self.hir.local(*l).ty.as_ref().and_then(|t| self.tuple_item(t)).ok_or("for in pattern type")?;
                            // A pair local takes its item's box, unboxed (#1486).
                            if self.pair_local(*l).is_none() && self.local_ty(*l)? != ty {
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
                let abi = CallAbi {
                    params: vec![self.b.value_ty(src)],
                    ret: MirTy::HeapRef,
                    ret_hi: None,
                };
                self.b.ins_call(target, vec![src], &abi).map_err(mir)?.0
            }
            (None, Items::Array) => src,
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
        let pinned = matches!(items, Items::Array) && self.cx.pinned_for(id);
        let item = self.b.ins_index(arr, cur, elem, pinned).map_err(mir)?;
        match (pat, parts) {
            (HirPat::Bind(local), _) if self.unit_local(*local) => {}
            (HirPat::Bind(local), _) => {
                let item = if self.for_mut_item(*local, elem, into_iter.is_none() && matches!(items, Items::Array)) {
                    // `for mut c in xs`: `c` names the item in `xs`, which
                    // the plan made unique before the loop; an item another
                    // name still shares is copied and stored back first, so
                    // the writes through `c` reach `xs` only (#2782).
                    let own = self.at_expr(id, |l| l.b.ins_cow(item, true).map_err(mir))?;
                    let own = self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![own]).map_err(mir)?;
                    self.b.ins_store_index(arr, cur, own, pinned).map_err(mir)?;
                    own
                } else {
                    item
                };
                let x = self.slot(*local)?;
                self.b.def_local(x, item).map_err(mir)?;
            }
            (HirPat::Tuple(_), None) if let Some(ty) = &nested_ty => self.bind_tuple_item(pat, item, Some(ty))?,
            (HirPat::Tuple(pats), Some(parts)) => {
                let mut parts = parts.into_iter();
                for (k, p) in pats.iter().enumerate() {
                    if let HirPat::Bind(l) = p
                        && !self.unit_local(*l)
                    {
                        let (l, ty) = parts.next().ok_or("for in pattern")?;
                        let v = self.b.ins_tuple_item(item, k as u32, ty).map_err(mir)?;
                        if self.pair_local(l).is_some() {
                            self.bind_item(l, v, None)?;
                        } else {
                            let slot = self.slot(l)?;
                            self.b.def_local(slot, v).map_err(mir)?;
                        }
                    }
                }
            }
            _ => return Err("for in pattern".into()),
        }
        self.counted_body(body, head, exit, i, MirTy::I64, MirConst::I64(1), None)
    }

    /// `for x in xs` over lane array `iterable`: each element's two words
    /// into pair local `x`.
    fn for_lanes(&mut self, pat: &HirPat, iterable: HirId, body: HirId, lane: HirLaneElem) -> Result<Val, Refusal> {
        // Where each word goes: a pair local's two slots, or a tuple
        // pattern's parts.
        let mut slots: [Option<(LocalId, MirTy)>; 2] = [None, None];
        match pat {
            HirPat::Bind(local) => {
                let (lo, hi, lo_ty, hi_ty) = self.pair_local(*local).ok_or("for in lane binding")?;
                slots = [Some((lo, lo_ty)), Some((hi, hi_ty))];
            }
            HirPat::Wild => {}
            HirPat::Tuple(pats) if pats.len() == 2 => {
                for (slot, p) in slots.iter_mut().zip(pats) {
                    match p {
                        HirPat::Wild => {}
                        HirPat::Bind(l) if self.unit_local(*l) => {}
                        HirPat::Bind(l) if self.pair_local(*l).is_none() => *slot = Some((self.slot(*l)?, self.local_ty(*l)?)),
                        _ => return Err("for in pattern".into()),
                    }
                }
            }
            _ => return Err("for in pattern".into()),
        }
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
        if slots.iter().any(Option::is_some) {
            let cur = self.b.use_local(i, MirTy::I64).map_err(mir)?;
            let (a, b) = self.b.ins_lane_index(arr, cur, lane.lo, lane.hi).map_err(mir)?;
            for (slot, w) in slots.into_iter().zip([a, b]) {
                if let Some((slot, ty)) = slot {
                    let w = self.fit(w, ty)?;
                    self.b.def_local(slot, w).map_err(mir)?;
                }
            }
        }
        self.counted_body(body, head, exit, i, MirTy::I64, MirConst::I64(1), None)
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

    /// [`Self::stack_select`] of an index that may be out of range: as HIR
    /// emission, an out-of-range index boxes the array and indexes that,
    /// which panics.
    fn stack_select_checked(&mut self, base: u32, n: usize, idx: ValueId, ty: MirTy) -> Result<ValueId, Refusal> {
        let zero = self.b.ins_const(MirConst::I64(0)).map_err(mir)?;
        let top = self.b.ins_const(MirConst::I64(n as i64)).map_err(mir)?;
        let neg = self.b.ins_cmp(MirCmpOp::Lt, idx, zero).map_err(mir)?;
        let oob = self.b.create_block();
        let low = self.b.create_block();
        let inb = self.b.create_block();
        self.b.branch(neg, oob, low).map_err(mir)?;
        self.switch(low);
        let over = self.b.ins_cmp(MirCmpOp::Ge, idx, top).map_err(mir)?;
        self.b.branch(over, oob, inb).map_err(mir)?;
        self.switch(inb);
        let v = self.stack_select(base, n, idx, ty)?;
        let in_end = self.cur;
        self.switch(oob);
        let mut words = Vec::with_capacity(n);
        for k in 0..n as u32 {
            words.push(self.b.use_local(LocalId(base + k), ty).map_err(mir)?);
        }
        let arr = self.b.ins_alloc(MirAllocKind::Array, words).map_err(mir)?;
        let arr = self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![arr]).map_err(mir)?;
        let w = self.b.ins_index(arr, idx, ty, false).map_err(mir)?;
        let oob_end = self.cur;
        let join = self.b.create_block();
        self.b.switch_to_block(in_end);
        self.jump(join)?;
        self.b.switch_to_block(oob_end);
        self.jump(join)?;
        self.switch(join);
        self.b.ins_stack_phi(vec![(in_end, v), (oob_end, w)]).map_err(mir)
    }

    /// Store `v` in stack array slots `base..base + n` at an index that may
    /// be out of range: as HIR emission, an out-of-range index boxes the
    /// array and stores into that, which panics.
    fn stack_store_checked(&mut self, base: u32, n: usize, idx: ValueId, v: ValueId, ty: MirTy) -> Result<(), Refusal> {
        let zero = self.b.ins_const(MirConst::I64(0)).map_err(mir)?;
        let top = self.b.ins_const(MirConst::I64(n as i64)).map_err(mir)?;
        let neg = self.b.ins_cmp(MirCmpOp::Lt, idx, zero).map_err(mir)?;
        let oob = self.b.create_block();
        let low = self.b.create_block();
        let inb = self.b.create_block();
        self.b.branch(neg, oob, low).map_err(mir)?;
        self.switch(low);
        let over = self.b.ins_cmp(MirCmpOp::Ge, idx, top).map_err(mir)?;
        self.b.branch(over, oob, inb).map_err(mir)?;
        self.switch(inb);
        self.stack_cases(base, n, idx, |l, slot| {
            l.b.def_local(slot, v).map_err(mir)?;
            Ok(None)
        })?;
        let in_end = self.cur;
        self.switch(oob);
        let mut words = Vec::with_capacity(n);
        for k in 0..n as u32 {
            words.push(self.b.use_local(LocalId(base + k), ty).map_err(mir)?);
        }
        let arr = self.b.ins_alloc(MirAllocKind::Array, words).map_err(mir)?;
        let arr = self.b.ins_gc_barrier(MirGcKind::Safepoint, vec![arr]).map_err(mir)?;
        self.b.ins_store_index(arr, idx, v, false).map_err(mir)?;
        let oob_end = self.cur;
        let join = self.b.create_block();
        self.b.switch_to_block(in_end);
        self.jump(join)?;
        self.b.switch_to_block(oob_end);
        self.jump(join)?;
        self.switch(join);
        Ok(())
    }

    /// `a[i] = v` on a stack array: the value first, then the index, as
    /// HIR emission stages them.
    fn stack_store(&mut self, place: HirId, base: HirId, index: HirId, value: HirId) -> Result<Val, Refusal> {
        let stack = match self.hir.expr(base).kind {
            HirKind::Local(local) if !self.boxes.contains_key(&local.0) => self.stack(local.0),
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
        let lit = matches!(self.hir.expr(index).kind, HirKind::Lit(Lit::Int(i)) if (0..n as i64).contains(&i));
        if !lit && !query::stack_index_proven(self.hir, place, index, n) {
            let Some(idx) = self.index_word(index)? else {
                return Ok(Val::Never);
            };
            self.stack_store_checked(slot, n, idx, v, ty)?;
            return Ok(Val::Unit);
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
    /// Bind one item of a `let` tuple pattern to `v`, of type `ty`: a
    /// nested tuple pattern (`let (a, (b, c)) = ..`) reads its items out
    /// of the heap tuple `v` is.
    fn bind_tuple_item(&mut self, pat: &HirPat, v: ValueId, ty: Option<&Ty>) -> Result<(), Refusal> {
        match pat {
            HirPat::Wild => Ok(()),
            // A `()` local has no slot (#1427).
            HirPat::Bind(local) if self.unit_local(*local) => Ok(()),
            HirPat::Bind(local) => self.bind_item(*local, v, ty),
            HirPat::Tuple(items) => {
                let Some(Ty::Tuple(tys)) = ty.map(strip_readonly) else {
                    return Err("let pattern".into());
                };
                if tys.len() != items.len() || self.b.value_ty(v) != MirTy::HeapRef {
                    return Err("let pattern".into());
                }
                for (k, (item, t)) in items.iter().zip(tys).enumerate() {
                    if matches!(item, HirPat::Wild) {
                        continue;
                    }
                    let item_ty = self.tuple_item(t).ok_or("let pattern")?;
                    let x = self.b.ins_tuple_item(v, k as u32, item_ty).map_err(mir)?;
                    self.bind_tuple_item(item, x, Some(t))?;
                }
                Ok(())
            }
            _ => Err("let pattern".into()),
        }
    }

    /// Bind `local` to the item `v` of type `ty` read from a tuple or
    /// array: a pair local's box unboxed to its two words (#1486).
    fn bind_item(&mut self, local: HirLocal, v: ValueId, ty: Option<&Ty>) -> Result<(), Refusal> {
        let Some((lo, hi, lo_ty, hi_ty)) = self.pair_local(local) else {
            return self.bind(local, v);
        };
        if self.b.value_ty(v) != MirTy::HeapRef {
            return Err("binding type".into());
        }
        let ty = ty.or(self.hir.local(local).ty.as_ref()).ok_or("local type")?.clone();
        let Val::Pair(a, b) = self.unbox_word(&ty, v, lo_ty, hi_ty)? else {
            return Err("binding type".into());
        };
        if self.b.value_ty(a) != lo_ty || self.b.value_ty(b) != hi_ty {
            return Err("binding type".into());
        }
        self.b.def_local(lo, a).map_err(mir)?;
        self.b.def_local(hi, b).map_err(mir)
    }

    fn bind(&mut self, local: HirLocal, v: ValueId) -> Result<(), Refusal> {
        let ty = self.local_ty(local)?;
        let v = match (self.b.value_ty(v), ty) {
            (from, to) if from == to => v,
            // A `bool` payload read from an `i64` pair word.
            (MirTy::I64, MirTy::Bool) => self.fit(v, ty)?,
            // So is an `f64` one (beside an `int` payload), as its bits.
            (MirTy::I64, MirTy::F64) => self.fit(v, ty)?,
            _ => return Err("binding type".into()),
        };
        if self.frame.slot(local).is_none() {
            let assigned = self.hir.exprs.iter().any(|e| {
                matches!(e.kind, HirKind::Assign { place, .. } if matches!(self.hir.expr(place).kind, HirKind::Local(l) if l == local))
            });
            if assigned {
                return Err("local slot".into());
            }
            self.aliases.insert(local.0, v);
            if let Some(&(home, None)) = self.pad.as_ref().and_then(|p| p.caps.get(&local.0)) {
                self.b.ins_home_store(home, v).map_err(mir)?;
            }
            return Ok(());
        }
        let slot = self.slot(local)?;
        self.keep_pin(local)?;
        self.b.def_local(slot, v).map_err(mir)
    }

    /// A variant, range or tuple built as two words.
    fn make_pair(&mut self, id: HirId, kind: &MakeKind, args: &[HirId]) -> Result<Val, Refusal> {
        let (lo_ty, hi_ty) = self.pair_tys(id).expect("pair word types");
        let (lo, hi) = match kind {
            MakeKind::Variant { tag: Some(tag), .. } => {
                let lo = match args {
                    [] => self.zero(lo_ty)?,
                    // A `()` payload: `0`, after a `()`-typed value's effect.
                    [arg] if query::is_unit_make(self.hir, *arg) || self.expr_ty(*arg).is_some_and(is_unit) => {
                        if !self.effect(*arg)? {
                            return Ok(Val::Never);
                        }
                        self.zero(lo_ty)?
                    }
                    // A two-word payload rides boxed.
                    [arg] => match self.word_or_boxed(*arg)? {
                        Some(v) if self.b.value_ty(v) == lo_ty => v,
                        Some(v) if lo_ty == MirTy::I64 && (self.b.value_ty(v).is_heap_word() || matches!(self.b.value_ty(v), MirTy::Bool | MirTy::F64)) => {
                            self.fit(v, lo_ty)?
                        }
                        Some(_) => return Err("variant payload type".into()),
                        None => return Ok(Val::Never),
                    },
                    _ => return Err("variant payload".into()),
                };
                (lo, self.b.ins_const(MirConst::I64(i64::from(*tag))).map_err(mir)?)
            }
            MakeKind::Range { .. } | MakeKind::Tuple | MakeKind::Class(_) => {
                let [a, b] = args else {
                    return Err("make pair".into());
                };
                let Some(a) = self.word_or_boxed(*a)? else {
                    return Ok(Val::Never);
                };
                let Some(b) = self.word_or_boxed(*b)? else {
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
    /// tag.
    fn match_(&mut self, scrutinee: HirId, arms: &[HirArm], tail: bool) -> Result<Val, Refusal> {
        // Arms up to the first catch-all; later ones never run.
        let reach = arms
            .iter()
            .position(|a| matches!(a.pat, HirPat::Wild | HirPat::Bind(_)))
            .map_or(arms.len(), |i| i + 1);
        let arms = &arms[..reach];
        if query::has_nested_test(arms) {
            return self.match_nested(scrutinee, arms, tail);
        }
        if let Some(consts) = self.scalar_arms(scrutinee, arms) {
            return self.match_scalar(scrutinee, arms, &consts, tail);
        }
        let boxed_local = match self.hir.expr(scrutinee).kind {
            HirKind::Local(l) => self.boxed_local(l),
            // A class field holds a two-word enum boxed.
            HirKind::Field { .. } => {
                (self.heap_field(scrutinee).is_some_and(|(_, w)| w == MirTy::HeapRef)
                    && self.boxed_niche(scrutinee).is_none())
                    || self.boxed_slot_field(scrutinee)
            }
            // So does an array element.
            // A lane array's keeps both words in place.
            HirKind::Index { base, kind: IndexKind::Array, .. } => {
                self.lane(base).is_none()
                    && self.heap_array(base)
                    && self.expr_ty(base).and_then(|t| self.elem_word(t)) == Some(MirTy::HeapRef)
                    && self.expr_ty(scrutinee).is_some_and(|t| self.cx.boxed_view(t, None).is_some())
            }
            _ => false,
        };
        // A two-word host result is matched on its two words.
        let host_box = self.host_pair(scrutinee) && !self.host_two_words(scrutinee);
        let boxed_host = host_box || self.boxed_call(scrutinee) || self.host_boxed(scrutinee) || self.ffi_boxed(scrutinee);
        if boxed_local || boxed_host || self.expr_ty(scrutinee).is_some_and(|t| self.cx.boxed_enum(t, None).is_some()) {
            return self.match_boxed(scrutinee, arms, tail);
        }
        if let Some(niche) = self.expr_ty(scrutinee).and_then(|t| self.cx.niche(t)) {
            return self.match_niche(scrutinee, arms, niche, tail);
        }
        // A value only ever kept boxed (an `Option` / `Result` whose
        // payload is two words itself): matched on its object.
        if self.pair_tys(scrutinee).is_none()
            && self.expr_ty(scrutinee).is_some_and(|t| self.word_ty(t).is_none() && self.cx.boxed_view(t, None).is_some())
        {
            return self.match_boxed(scrutinee, arms, tail);
        }
        let (payload, tag) = match self.value(scrutinee)? {
            Val::Pair(payload, tag) => (payload, tag),
            Val::Never => return Ok(Val::Never),
            // A value that comes out boxed (an inner `r?` whose `Result`
            // was a boxed payload, #1166): matched on its object.
            Val::V(obj)
                if self.b.value_ty(obj) == MirTy::HeapRef
                    && let Some(ty) = self.expr_ty(scrutinee).cloned()
                    && self.cx.boxed_view(&ty, None).is_some() =>
            {
                return self.match_boxed_obj(obj, &ty, arms, tail);
            }
            _ => return Err("match scrutinee".into()),
        };
        // `Some(x) => x, None => 0`: a unit variant's payload word is `0`,
        // so the value is the payload whatever the tag.
        let zero_arm = |arm: &HirArm| {
            matches!(arm.pat, HirPat::Variant { fields: HirPatFields::Unit, .. })
                && matches!(self.hir.expr(arm.body).kind, HirKind::Lit(Lit::Int(0)))
        };
        if let [a, b] = arms
            && (zero_arm(a) && query::is_identity_arm(self.hir, b) || zero_arm(b) && query::is_identity_arm(self.hir, a))
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
            match arm.pat {
                // `rest => …` binds the whole value (#1161): the pair as
                // two words, or boxed to one.
                HirPat::Bind(local) if i == last => self.bind_whole_pair(scrutinee, local, payload, tag)?,
                _ => self.bind_payload(&arm.pat, payload)?,
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
        // A last arm that only exits (`?`'s `Err(e) => return Err(e)`) is cold.
        if let Some(start) = last_start
            && self.cold_arm(arms[last].body)
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
        self.match_boxed_obj(obj, &ty, arms, tail)
    }

    /// [`Self::match_boxed`] on the scrutinee's object, already lowered.
    fn match_boxed_obj(&mut self, obj: ValueId, ty: &Ty, arms: &[HirArm], tail: bool) -> Result<Val, Refusal> {
        let ty = ty.clone();
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

    /// A `match` whose patterns test inside a payload: the scrutinee boxed,
    /// then each arm's tests in turn,
    /// a miss falling to the next arm. The last arm tests nothing: the
    /// checker proved the match exhaustive.
    fn match_nested(&mut self, scrutinee: HirId, arms: &[HirArm], tail: bool) -> Result<Val, Refusal> {
        let ty = self.expr_ty(scrutinee).ok_or("match scrutinee")?.clone();
        if self.cx.boxed_view(&ty, None).is_none() {
            return Err("nested match type".into());
        }
        let Some(obj) = self.boxed_word(scrutinee)? else {
            return Ok(Val::Never);
        };
        if self.b.value_ty(obj) != MirTy::HeapRef {
            return Err("nested match scrutinee".into());
        }
        if let Some(tree) = crate::hir::match_tree::outer_groups(arms)
            && tree.groups.iter().all(|g| self.cx.scalar_variant(g.enum_name, g.variant).is_none())
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

    /// A nested match as a decision tree on its outer tag: one
    /// `JumpIfMatch` per variant, then
    /// that variant's rows test only their sub-patterns. A group whose rows
    /// all miss runs the catch-all, emitted once.
    fn match_grouped(&mut self, tree: &crate::hir::match_tree::OuterTree<'_>, obj: ValueId, ty: &Ty, tail: bool) -> Result<Val, Refusal> {
        let catch = tree.catch_all.map(|_| self.b.create_block());
        let tested = if catch.is_some() { tree.groups.len() } else { tree.groups.len() - 1 };
        let mut groups = Vec::with_capacity(tree.groups.len());
        for (gi, g) in tree.groups.iter().enumerate() {
            let field_tys = self.cx.boxed_view(ty, Some(g.variant)).ok_or("nested variant payload")?;
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
                        if matches!(sub, HirPat::Variant { .. }) && self.cx.niche(t).is_some() {
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
            HirPatFields::Record(named) => self.cx.payload_names(enum_name, variant)
                .iter()
                .map(|name| named.iter().find(|(n, _)| n == name).map(|(_, p)| p))
                .collect(),
        }
    }

    /// The word a boxed variant's payload field of type `t` reads as.
    fn payload_word_ty(&self, t: &Ty) -> Result<MirTy, Refusal> {
        self.word_ty(t)
            .or_else(|| is_unit(t).then_some(MirTy::I64))
            .or_else(|| self.cx.boxed_view(t, None).map(|_| MirTy::HeapRef))
            .or_else(|| is_heap_tuple(t).then_some(MirTy::HeapRef))
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
                if let Some(lit) = self.cx.scalar_variant(enum_name, variant) {
                    let Some(miss) = miss else {
                        return Ok(());
                    };
                    let ScalarLit::Const(c @ MirConst::I64(_)) = lit else {
                        return Err("nested scalar pattern".into());
                    };
                    if self.b.value_ty(v) != MirTy::I64 {
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
                let field_tys = self.cx.boxed_view(ty, Some(variant)).ok_or("nested variant payload")?;
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
                        if matches!(sub, HirPat::Variant { .. }) && self.cx.niche(t).is_some() {
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

    /// `match` on a pointer-niche word: the payload side falls through and
    /// is laid out
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
        if is_niche(self.b.value_ty(w)) && self.b.value_ty(w) != niche {
            return Err("match niche word".into());
        }
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
        if self.cold_arm(first.body) {
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
                let binds = query::arm_fields(self.hir, &arm.pat).map_err(|e| e.to_string())?;
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
            let w = this.b.ins_cast(MirCastKind::Word, niche, z).map_err(mir)?;
            this.known_niche.insert(w, (false, None));
            Ok(Val::V(w))
        };
        let arg = match args {
            [] => return zero(self),
            // `Ok(e)` of a `()`-typed `e`: `e` for its effect, then `Ok(())`.
            [arg] if query::is_unit_make(self.hir, *arg) || self.expr_ty(*arg).is_some_and(is_unit) => {
                if !self.effect(*arg)? {
                    return Ok(Val::Never);
                }
                // A `Result<unit, E>` is a zero niche, never a `Result` one.
                if niche == MirTy::NicheRes {
                    return Err("unit payload in a Result niche".into());
                }
                return zero(self);
            }
            [arg] => *arg,
            _ => return Err("variant payload".into()),
        };
        // A two-word payload (a tuple) rides boxed.
        let Some(v) = self.word_or_boxed(arg)? else {
            return Ok(Val::Never);
        };
        if !self.b.value_ty(v).is_heap_word() {
            return Err("niche payload type".into());
        }
        let err = niche == MirTy::NicheRes && variant == "Err";
        // `Err(e) => return Err(e)`: the matched word as it was (a body
        // returning it boxed makes the known variant instead).
        if let Some(&(w, untagged)) = self.unniche.get(&v)
            && untagged == err
            && self.b.value_ty(w) == niche
            && self.ret_boxed_niche.is_none()
        {
            return Ok(Val::V(w));
        }
        let w = if err {
            let one = self.b.ins_const(MirConst::I64(1)).map_err(mir)?;
            self.b.ins_binop(MirBinOp::BitOr, v, one).map_err(mir)?
        } else {
            self.b.ins_cast(MirCastKind::Word, niche, v).map_err(mir)?
        };
        // `Ok` of a niche `Result` is its first variant; `Some` and `Err`
        // of a zero niche the second.
        let second = !(niche == MirTy::NicheRes && variant == "Ok");
        self.known_niche.insert(w, (second, Some(v)));
        Ok(Val::V(w))
    }

    /// A boxed enum variant pattern's payload word types and the local each
    /// word binds (`None` for `_` or a `()` field).
    fn variant_payload(&self, ty: &Ty, pat: &HirPat) -> Result<(Vec<MirTy>, Vec<Option<HirLocal>>), Refusal> {
        let HirPat::Variant { variant, .. } = pat else {
            return Err("match arm pattern".into());
        };
        let fields = self.cx.boxed_view(ty, Some(variant)).ok_or("variant payload")?;
        let binds = query::arm_fields(self.hir, pat).map_err(|e| e.to_string())?;
        if !binds.is_empty() && binds.len() != fields.len() {
            return Err("payload pattern".into());
        }
        let binds = if binds.is_empty() { vec![None; fields.len()] } else { binds };
        let tys = fields
            .iter()
            .zip(&binds)
            .map(|(t, bind)| {
                self.word_ty(t)
                    .or_else(|| is_unit(t).then_some(MirTy::I64))
                    .or_else(|| self.cx.boxed_view(t, None).map(|_| MirTy::HeapRef))
                    .or_else(|| is_heap_tuple(t).then_some(MirTy::HeapRef))
                    // A field of a type never resolved (a thread's or
                    // channel's value) that the arm ignores: its word is
                    // unpacked, as emission's, and never read.
                    .or_else(|| (bind.is_none() && is_var(t)).then_some(MirTy::I64))
            })
            .collect::<Option<Vec<_>>>()
            .ok_or("payload type")?;
        Ok((tys, binds))
    }

    /// Each arm's constant when `match` tests an `int`, or a scalar enum
    /// backed by an `int` or `float`, against literals (`None` for a
    /// catch-all), as `query::is_scalar_match`.
    fn scalar_arms(&self, scrutinee: HirId, arms: &[HirArm]) -> Option<Vec<Option<ScalarLit>>> {
        let ty = self.expr_ty(scrutinee)?;
        let int = matches!(strip_readonly(ty), Ty::Con(n) if n == crate::typechecking::ty::INT);
        if !int && self.cx.scalar_enum(ty).is_none() {
            return None;
        }
        arms.iter()
            .map(|arm| match &arm.pat {
                HirPat::Wild | HirPat::Bind(_) => Some(None),
                HirPat::Int(n) if int => Some(Some(ScalarLit::Const(MirConst::I64(*n)))),
                HirPat::Variant {
                    enum_name, variant, fields, ..
                } if !int && unit_fields(fields) => self.cx.scalar_variant(enum_name, variant).map(Some),
                _ => None,
            })
            .collect()
    }

    /// `match` of a scalar word on constants: one equality test per arm but
    /// the last, or a binary search over many `int` literals.
    fn match_scalar(&mut self, scrutinee: HirId, arms: &[HirArm], consts: &[Option<ScalarLit>], tail: bool) -> Result<Val, Refusal> {
        let Some(v) = self.word(scrutinee)? else {
            return Ok(Val::Never);
        };
        let literal = |pat: &HirPat| {
            let i = arms.iter().position(|a| std::ptr::eq(&a.pat, pat))?;
            match consts[i] {
                Some(ScalarLit::Const(MirConst::I64(n))) => Some(n),
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
                Some(lit) if i < last => {
                    let hit = self.scalar_eq(v, lit)?;
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
            && self.cold_arm(arms[last].body)
        {
            self.mark_cold(start);
        }
        self.join_arms(out)
    }

    /// A scalar literal's word: its constant, or its string.
    fn scalar_lit(&mut self, lit: ScalarLit) -> Result<ValueId, Refusal> {
        match lit {
            ScalarLit::Const(c) => self.b.ins_const(c).map_err(mir),
            ScalarLit::Str(idx) => self.b.ins_string(idx).map_err(mir),
        }
    }

    /// Whether the word `v` is `lit`, as HIR emission's `EQ` against the
    /// pushed backing: words compare as words (a `bool` as its `0` / `1`),
    /// a string by content.
    fn scalar_eq(&mut self, v: ValueId, lit: ScalarLit) -> Result<ValueId, Refusal> {
        let k = self.scalar_lit(lit)?;
        match (self.b.value_ty(v), self.b.value_ty(k)) {
            (MirTy::HeapRef, MirTy::HeapRef) => self.b.ins_str_eq(v, k, false).map_err(mir),
            (MirTy::Bool, MirTy::Bool) => {
                let v = self.b.ins_cast(MirCastKind::Word, MirTy::I64, v).map_err(mir)?;
                let k = self.b.ins_cast(MirCastKind::Word, MirTy::I64, k).map_err(mir)?;
                self.b.ins_cmp(MirCmpOp::Eq, v, k).map_err(mir)
            }
            (a, b) if a == b && a.is_numeric() => self.b.ins_cmp(MirCmpOp::Eq, v, k).map_err(mir),
            _ => Err("scalar match type".into()),
        }
    }

    /// Search the sorted `cases` for `v`: split on the
    /// middle literal with `<` (code `LE`), and test a small leaf case by
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
    /// A binding arm's local as the whole `[payload, tag]` scrutinee.
    fn bind_whole_pair(&mut self, scrutinee: HirId, local: HirLocal, payload: ValueId, tag: ValueId) -> Result<(), Refusal> {
        if self.unit_local(local) {
            return Ok(());
        }
        if let Some((lo, hi, lo_ty, hi_ty)) = self.pair_local(local) {
            if self.b.value_ty(payload) != lo_ty || self.b.value_ty(tag) != hi_ty {
                return Err("binding pair type".into());
            }
            self.b.def_local(lo, payload).map_err(mir)?;
            return self.b.def_local(hi, tag).map_err(mir);
        }
        let ty = self.expr_ty(scrutinee).ok_or("match scrutinee")?.clone();
        let Val::V(obj) = self.box_pair(&ty, payload, tag)? else {
            return Err("binding pair".into());
        };
        self.bind(local, obj)
    }

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
        if self.unit_local(bound) {
            return Ok(());
        }
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

    /// An arm to lay out after the body: one that only exits or fails
    /// ([`query::cold_exit`]), or one that calls a cold function.
    fn cold_arm(&self, id: HirId) -> bool {
        query::cold_exit(self.hir, id) || self.calls_cold(id)
    }

    /// Whether `id`'s subtree calls a cold function (`#[cold]`, or one
    /// that never returns).
    fn calls_cold(&self, id: HirId) -> bool {
        if !self.cx.cold_ok() {
            return false;
        }
        let mut stack = vec![id];
        while let Some(at) = stack.pop() {
            // Only a call or an operator has a call site.
            if matches!(self.hir.expr(at).kind, HirKind::Call { .. } | HirKind::Bin { .. } | HirKind::Un { .. })
                && self.site(at).call.as_ref().is_some_and(|site| self.cx.cold_target(site.target))
            {
                return true;
            }
            stack.extend(self.hir.children(at));
        }
        false
    }

    /// Lay out the blocks lowering started since `order[start]` after the
    /// body; a range nested in one already marked goes with it.
    fn mark_cold(&mut self, start: usize) {
        if self.cx.cold_ok() {
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
        let discard = self.discard_arms.contains(&body);
        if discard {
            self.drop_value(body);
        }
        let val = if tail { self.ret_value(body)? } else { self.value(body)? };
        match val {
            Val::Never => Ok(Val::Never),
            _ if discard => Ok(Val::Unit),
            val if tail => {
                self.ret(val)?;
                Ok(Val::Never)
            }
            val => {
                if let (Val::V(_), Some(ty)) = (val, self.expr_ty(body).cloned()) {
                    self.arm_tys.insert(self.cur, ty);
                }
                Ok(val)
            }
        }
    }

    fn if_(&mut self, cond: HirId, then: HirId, els: Option<HirId>, tail: bool) -> Result<Val, Refusal> {
        // `if !c` branches on `c` with the edges swapped, as HIR lowering
        // inverts it.
        let (cond, negated) = match self.hir.expr(cond).kind {
            HirKind::Un { op: UnOp::Not, operand } => (operand, true),
            _ => (cond, false),
        };
        let Some(mut c) = self.word(cond)? else {
            return Ok(Val::Never);
        };
        // An int condition branches as the runtime's `JMPF` reads a word: true
        // when its low byte is `1`.
        if self.b.value_ty(c) == MirTy::I64 {
            let mask = self.b.ins_const(MirConst::I64(0xff)).map_err(mir)?;
            let low = self.b.ins_binop(MirBinOp::BitAnd, c, mask).map_err(mir)?;
            let one = self.b.ins_const(MirConst::I64(1)).map_err(mir)?;
            c = self.b.ins_cmp(MirCmpOp::Eq, low, one).map_err(mir)?;
        }
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
            // An arm that calls a cold function is cold either way.
            let exit_arm = if els.is_some() { arm == els } else { arm == Some(then) };
            if (matches!(val, Val::Never) && exit_arm && arm.is_some_and(|a| self.cold_arm(a)))
                || arm.is_some_and(|a| self.calls_cold(a))
            {
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
            if self.cold_arm(then) {
                self.mark_cold(start);
            }
        } else {
            self.jump(join)?;
            if self.calls_cold(then) {
                self.mark_cold(start);
            }
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
        let arms = self.unbox_arms(arms)?;
        let join = self.b.create_block();
        for &(block, _) in &arms {
            self.b.switch_to_block(block);
            self.jump(join)?;
        }
        self.switch(join);
        self.join(arms)
    }

    /// Arms meeting where some give a pair and others a boxed word (an
    /// `Option` payload of a `Result` beside an `Option::None` built
    /// boxed): each boxed arm unboxes to the pair at its end, as HIR
    /// emission's join converts it.
    fn unbox_arms(&mut self, arms: Vec<(BlockId, Val)>) -> Result<Vec<(BlockId, Val)>, Refusal> {
        let Some((lo_ty, hi_ty)) = arms.iter().find_map(|&(_, v)| match v {
            Val::Pair(lo, hi) => Some((self.b.value_ty(lo), self.b.value_ty(hi))),
            _ => None,
        }) else {
            return Ok(arms);
        };
        if !arms.iter().any(|&(_, v)| matches!(v, Val::V(w) if self.b.value_ty(w) == MirTy::HeapRef)) {
            return Ok(arms);
        }
        let mut out = Vec::with_capacity(arms.len());
        for (block, val) in arms {
            match (val, self.arm_tys.get(&block).cloned()) {
                (Val::V(v), Some(ty)) if self.b.value_ty(v) == MirTy::HeapRef => {
                    self.switch(block);
                    let val = self.unbox_word(&ty, v, lo_ty, hi_ty)?;
                    out.push((self.cur, val));
                }
                arm => out.push((block, arm.0)),
            }
        }
        Ok(out)
    }
}

/// A short name for a construct MIR lowering does not take: a `for`'s
/// iteration kind, else [`HirKind::name`].
fn kind_name(kind: &HirKind) -> &'static str {
    match kind {
        HirKind::ForIn { kind, pat, .. } => match kind {
            Some(ForInKind::Array) if !matches!(pat, HirPat::Bind(_)) => "for in array pattern",
            Some(ForInKind::Array) => "for in array",
            Some(ForInKind::Tuple { .. }) => "for in tuple",
            Some(ForInKind::Coroutine) => "for in coroutine",
            Some(ForInKind::Custom { counted: Some(_), .. }) => "for in counted custom",
            Some(ForInKind::Custom { .. }) => "for in iterator",
            _ => "for in",
        },
        other => other.name(),
    }
}
