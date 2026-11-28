//! MIR function / block containers and SSA verification.

use std::collections::{BTreeSet, HashMap, HashSet};

use common::DebugLoc;

use super::inst::{BlockId, LocalId, MirConst, MirInst, Terminator, ValueId};
use crate::layout::MirLayout;
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
    /// Types of result words 3.. of a body that returns more than two to
    /// its direct calls (`Terminator::Return::more`, `MirInst::Call::dest_more`).
    /// Empty unless `ret_hi_ty` is set; at most `MAX_RET_MORE` entries.
    pub ret_more_tys: Vec<MirTy>,
    pub ret_layout: MirLayout,
    pub entry: BlockId,
    pub blocks: Vec<MirBlock>,
    /// Type of each allocated value, indexed by [`ValueId::index`].
    pub types: Vec<MirTy>,
    /// Dest → source loc from the IL op that produced it (C3 sparse DebugLoc).
    pub value_locs: HashMap<ValueId, DebugLoc>,
    /// A call's or trapping op's own loc where a variable's defining store
    /// retagged its value with the variable's def site: a panic there
    /// reports the op's line (#841, #885).
    pub op_locs: HashMap<ValueId, DebugLoc>,
    /// Block terminator loc (Return / Jump / Br).
    pub term_locs: HashMap<BlockId, DebugLoc>,
    /// Last SSA def of each IL local after construction (named-let remap).
    pub debug_slots: HashMap<LocalId, ValueId>,
    /// A debugger reads the body's locals: each `debug_slots` value keeps
    /// a frame word of its own, never an inline operand.
    pub debug_keep: bool,
    /// A coverage compile counts a line covered when code it emits runs:
    /// the body keeps the code of each line instead of
    /// folding it away.
    pub keep_lines: bool,
    /// An `i64` may be a type variable's word, a raw scalar or a box: the
    /// objects this body makes carry no word kinds.
    pub opaque_words: bool,
    /// Locals (frame slots, for MIR lowered from HIR) a heap word was
    /// stored in: another body of the same HIR keeps them there.
    pub heap_slots: BTreeSet<LocalId>,
    /// Locals a non-heap word was stored in.
    pub word_slots: BTreeSet<LocalId>,
    /// Code HIR emission adds that this MIR (lowered from HIR) leaves out,
    /// such as an existential pack: the MIR still describes the body's
    /// frame slots (its GC map), but no tier emits code from it.
    pub emit_refusal: Option<&'static str>,
    /// Word type of each home ([`MirInst::HomeLoad`] / [`MirInst::HomeStore`]):
    /// frame words past the registers, a `defer`'s flag or capture each.
    pub homes: Vec<MirTy>,
    /// Frame word of home 0 when another body's frame holds the homes (a
    /// cleanup pad reads its function's); else past this body's registers.
    pub home_base: Option<u8>,
    /// The unwind cleanup pad of a body with `defer`s: entered at a panic
    /// in this body's frame, it runs the armed `defer`s from the homes and
    /// resumes the unwind. Emitted after the body, in its frame.
    pub cleanup: Option<Box<MirFunc>>,
    /// Callees a call of this body may reach as a `TailCall` (planned tail
    /// calls of a `return`): a call to one whose result the block returns.
    pub tail_targets: HashSet<crate::bodies::Label>,
    /// Calls of a pure function returning one scalar word: CSE and LICM
    /// may treat them as operators that can trap.
    pub pure_calls: HashSet<ValueId>,
    /// `ProductLoad`s of a `readonly class` instance's field: no store
    /// or call changes the word, so LICM and CSE treat them as operators
    /// on the object (#1560).
    pub invariant_loads: HashSet<ValueId>,
    /// `Index` reads of a `byte` element (a `Vec<byte>` or `[byte; N]`):
    /// `byte_scan` reads them as byte lanes.
    pub byte_loads: HashSet<ValueId>,
    /// `Index` reads of a heap word in a forward scan: dense emission
    /// asks the runtime to prefetch the object a few elements ahead
    /// ([`super::mark_prefetch_scans`]).
    pub prefetch_loads: HashSet<ValueId>,
    /// Calls of a function that never resizes an array (it may write
    /// elements and fields).
    pub steady_calls: HashSet<ValueId>,
    /// No code that runs at an allocation (a finalizer) resizes an array:
    /// an allocation leaves every length as it was.
    pub lengths_steady: bool,
    /// Heap words a local was bound to (each `def_local`, and the φs that
    /// merge a local's definitions). The frame holds a local's word until
    /// it is rebound or the function returns, so emission zeroes past
    /// their last use only the registers of temporaries (#1650).
    pub bound_words: HashSet<ValueId>,
    /// The words locals were bound to, by local (each `def_local`, and the
    /// φs merging a local's definitions). The frame holds one from its
    /// definition until another of the same local's takes its place.
    pub local_binds: Vec<(LocalId, ValueId)>,
    /// This body returns a two-word result as its one word (its box), a
    /// function value's entry; `pair_twin` is the same function taking
    /// the same words and returning the two words. A
    /// [`MirInst::CallIndirect`] of a two-word result enters the twin.
    pub pair_twin: Option<crate::bodies::Label>,
}

/// The instruction defining each value (its [`MirInst::dest`]) and its
/// block ([`MirFunc::defs`]).
pub type Defs<'a> = HashMap<ValueId, (BlockId, &'a MirInst)>;

/// The `int` constant `v` is, when [`Defs`] says so.
pub fn const_i64(defs: &Defs, v: ValueId) -> Option<i64> {
    match defs.get(&v) {
        Some((_, MirInst::Const { c: MirConst::I64(k), .. })) => Some(*k),
        _ => None,
    }
}

/// Where `v` ends up under `subst`: its replacement's replacement, and
/// so on. A cycle stops at the first value seen twice.
pub fn resolve(subst: &HashMap<ValueId, ValueId>, start: ValueId) -> ValueId {
    let (mut v, mut steps) = (start, 0);
    while let Some(&n) = subst.get(&v) {
        if n == v {
            break;
        }
        steps += 1;
        if steps > subst.len() {
            return resolve_cycle(subst, start);
        }
        v = n;
    }
    v
}

#[cold]
fn resolve_cycle(subst: &HashMap<ValueId, ValueId>, mut v: ValueId) -> ValueId {
    let mut seen = HashSet::new();
    while let Some(&n) = subst.get(&v) {
        if n == v || !seen.insert(v) {
            break;
        }
        v = n;
    }
    v
}

impl MirFunc {
    /// Drop the spare capacity building left in this body's vectors (about
    /// half its instruction storage): it is kept until the program lowers
    /// (docs/internals/compile-memory.md). Maps keep theirs, since
    /// resizing one reorders its iteration.
    pub fn shrink_to_fit(&mut self) {
        self.params.shrink_to_fit();
        self.blocks.shrink_to_fit();
        for b in &mut self.blocks {
            b.insts.shrink_to_fit();
        }
        self.types.shrink_to_fit();
        self.homes.shrink_to_fit();
        if let Some(c) = &mut self.cleanup {
            c.shrink_to_fit();
        }
    }

    /// The homes that own a count, as a mask (bit `h` for home `h`): the
    /// ones this body's cleanup pad loads and releases. A generator's
    /// suspended frame owns exactly these words, so the cycle collector
    /// can see through it. `None` when a counted home is past the 64th.
    pub fn counted_homes(&self) -> Option<u64> {
        Self::counted_homes_of(self.cleanup.as_deref())
    }

    /// [`Self::counted_homes`] of the body whose cleanup pad is `pad`.
    pub fn counted_homes_of(pad: Option<&MirFunc>) -> Option<u64> {
        let Some(pad) = pad else { return Some(0) };
        let mut loaded: HashMap<ValueId, u16> = HashMap::new();
        let mut mask = 0u64;
        for inst in pad.blocks.iter().flat_map(|b| &b.insts) {
            match inst {
                MirInst::HomeLoad { dest, home } => {
                    loaded.insert(*dest, *home);
                }
                MirInst::RcDec { src, .. } => {
                    if let Some(&h) = loaded.get(src) {
                        if h >= 64 {
                            return None;
                        }
                        mask |= 1 << h;
                    }
                }
                _ => {}
            }
        }
        Some(mask)
    }

    /// The function entries this body's code names: the callees of its
    /// calls, its code pointers and function values, its cleanup pad's
    /// included.
    pub fn code_targets(&self) -> Vec<crate::bodies::Label> {
        use super::MirAllocKind;
        let mut out = Vec::new();
        for inst in self.blocks.iter().flat_map(|b| &b.insts) {
            match inst {
                MirInst::Call { target, .. }
                | MirInst::CodePtr { target, .. }
                | MirInst::CallIndirect {
                    pair: Some((_, target)), ..
                } => out.push(*target),
                MirInst::Alloc {
                    kind:
                        MirAllocKind::Coro { target, .. }
                        | MirAllocKind::Fn { target, .. }
                        | MirAllocKind::PolyFn { target, .. }
                        | MirAllocKind::PolyFnCapture { target, .. },
                    ..
                } => out.push(*target),
                _ => {}
            }
        }
        out.extend(self.pair_twin);
        if let Some(pad) = &self.cleanup {
            out.extend(pad.code_targets());
        }
        out
    }

    /// A pass replaced values: each named local follows its value to the
    /// one that replaces it.
    /// Rename every value an instruction or terminator reads, and the
    /// debug slots, through `map`. Dests stay as they are.
    pub fn map_values(&mut self, map: impl Fn(ValueId) -> ValueId) {
        for block in &mut self.blocks {
            for inst in &mut block.insts {
                inst.rewrite_values(&map);
            }
            if let Some(term) = &mut block.term {
                term.rewrite_values(&map);
            }
        }
        self.rewrite_debug_slots(map);
    }

    /// [`Self::map_values`] through `subst`, following chains to their end
    /// ([`resolve`]).
    pub fn replace_uses(&mut self, subst: &HashMap<ValueId, ValueId>) {
        if !subst.is_empty() {
            self.map_values(|v| resolve(subst, v));
        }
    }

    /// Every value's defining instruction, for many lookups.
    pub fn defs(&self) -> Defs<'_> {
        self.blocks.iter().flat_map(|b| b.insts.iter().map(move |i| (i.dest(), (b.id, i)))).collect()
    }

    /// The instruction defining `v` and its block. A scan of the body: for
    /// more than a few lookups build [`Self::defs`].
    pub fn def_of(&self, v: ValueId) -> Option<(BlockId, &MirInst)> {
        self.blocks.iter().find_map(|b| b.insts.iter().find(|i| i.dest() == v).map(|i| (b.id, i)))
    }

    /// The instruction defining `v` (a scan, as [`Self::def_of`]).
    pub fn def_inst(&self, v: ValueId) -> Option<&MirInst> {
        self.def_of(v).map(|(_, i)| i)
    }

    /// The constant `v` is, when a `Const` defines it (a scan, as
    /// [`Self::def_of`]).
    pub fn const_of(&self, v: ValueId) -> Option<MirConst> {
        match self.def_of(v) {
            Some((_, MirInst::Const { c, .. })) => Some(*c),
            _ => None,
        }
    }

    /// The `int` constant `v` is (a scan, as [`Self::def_of`]).
    pub fn const_i64(&self, v: ValueId) -> Option<i64> {
        match self.const_of(v) {
            Some(MirConst::I64(k)) => Some(k),
            _ => None,
        }
    }

    pub fn rewrite_debug_slots(&mut self, map: impl Fn(ValueId) -> ValueId) {
        for v in self.debug_slots.values_mut() {
            *v = map(*v);
        }
        let renamed: Vec<ValueId> = self.bound_words.iter().map(|&v| map(v)).collect();
        self.bound_words.extend(renamed);
        for (_, v) in &mut self.local_binds {
            *v = map(*v);
        }
    }

    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            params: Vec::new(),
            ret_ty: None,
            ret_hi_ty: None,
            ret_more_tys: Vec::new(),
            ret_layout: MirLayout::Word,
            entry: BlockId(0),
            blocks: vec![MirBlock::new(BlockId(0))],
            types: Vec::new(),
            value_locs: HashMap::new(),
            op_locs: HashMap::new(),
            term_locs: HashMap::new(),
            debug_slots: HashMap::new(),
            debug_keep: false,
            keep_lines: false,
            heap_slots: BTreeSet::new(),
            word_slots: BTreeSet::new(),
            opaque_words: false,
            emit_refusal: None,
            homes: Vec::new(),
            home_base: None,
            cleanup: None,
            tail_targets: HashSet::new(),
            pure_calls: HashSet::new(),
            invariant_loads: HashSet::new(),
            byte_loads: HashSet::new(),
            prefetch_loads: HashSet::new(),
            steady_calls: HashSet::new(),
            lengths_steady: true,
            bound_words: HashSet::new(),
            local_binds: Vec::new(),
            pair_twin: None,
        }
    }

    pub fn loc_of(&self, v: ValueId) -> DebugLoc {
        self.value_locs
            .get(&v)
            .copied()
            .unwrap_or_else(DebugLoc::unknown)
    }

    /// The location of the op that produced `v` itself (see `op_locs`).
    pub fn op_loc(&self, v: ValueId) -> DebugLoc {
        self.op_locs.get(&v).copied().unwrap_or_else(|| self.loc_of(v))
    }

    pub fn term_loc(&self, b: BlockId) -> DebugLoc {
        self.term_locs
            .get(&b)
            .copied()
            .unwrap_or_else(DebugLoc::unknown)
    }

    pub fn ty(&self, v: ValueId) -> MirTy {
        self.types.get(v.index()).copied().unwrap_or(MirTy::Bottom)
    }

    /// `v`'s type as the verifier checks an operand: a
    /// [`MirTy::MaybeObj`] word stands wherever an `i64` does
    /// ([`MirTy::word_class`]).
    fn vty(&self, v: ValueId) -> MirTy {
        self.ty(v).word_class()
    }

    #[cfg(test)]
    /// True when any HostInvoke is impure (I6).
    pub fn has_impure_host(&self) -> bool {
        self.blocks.iter().any(|b| {
            b.insts.iter().any(|i| match i {
                MirInst::HostInvoke { native_id, .. } => !super::effects::host_is_pure(*native_id),
                MirInst::Ffi { .. } | MirInst::ExternCall { .. } => true,
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

    /// Lay the blocks out in `layout` order, numbered as they come; a
    /// block not listed is dropped, with the φ incomings from it.
    pub fn relayout(&mut self, layout: &[BlockId]) {
        let mut new_id = vec![None; self.blocks.len()];
        for (i, b) in layout.iter().enumerate() {
            new_id[b.index()] = Some(BlockId(i as u32));
        }
        let map = |b: BlockId| new_id[b.index()].expect("block in the layout");
        let mut old: Vec<Option<MirBlock>> = std::mem::take(&mut self.blocks).into_iter().map(Some).collect();
        for &b in layout {
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
            self.blocks.push(block);
        }
        self.entry = map(self.entry);
        self.term_locs = std::mem::take(&mut self.term_locs)
            .into_iter()
            .filter_map(|(b, loc)| Some((new_id.get(b.index()).copied().flatten()?, loc)))
            .collect();
    }

    pub fn block(&self, id: BlockId) -> &MirBlock {
        &self.blocks[id.index()]
    }

    pub fn block_mut(&mut self, id: BlockId) -> &mut MirBlock {
        &mut self.blocks[id.index()]
    }

    /// The predecessors of one block (as [`Self::preds`] lists them),
    /// without building every block's list.
    pub fn preds_of(&self, block: BlockId) -> Vec<BlockId> {
        self.blocks
            .iter()
            .filter(|b| b.term.as_ref().is_some_and(|t| t.targets(block)))
            .map(|b| b.id)
            .collect()
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
        if !self.ret_more_tys.is_empty() && self.ret_hi_ty.is_none() {
            return Err("result words past the second without a second".into());
        }
        if self.ret_more_tys.len() > crate::mir::MAX_CALL_RET_MORE {
            return Err(format!("function returns {} words", 2 + self.ret_more_tys.len()));
        }
        let mut defined = vec![false; self.types.len()];
        for &p in &self.params {
            if p.index() >= self.types.len() {
                return Err(format!("param {p} has no type"));
            }
            if !self.vty(p).is_specialized() {
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
                    if self.vty(*cond) != MirTy::Bool {
                        return Err(format!("br cond {} is {}", cond, self.vty(*cond)));
                    }
                }
                Terminator::JumpIfMatch {
                    scrutinee,
                    tag,
                    payloads,
                    ..
                } => {
                    let _ = tag;
                    if !self.vty(*scrutinee).is_specialized() {
                        return Err(format!(
                            "JumpIfMatch scrutinee {} is {}",
                            scrutinee,
                            self.vty(*scrutinee)
                        ));
                    }
                    for p in payloads {
                        if !self.vty(*p).is_specialized() {
                            return Err(format!("JumpIfMatch payload {p} is {}", self.vty(*p)));
                        }
                    }
                }
                Terminator::Return { lo, hi, more } => {
                    if hi.is_some() && lo.is_none() {
                        return Err("two-slot return missing payload word".into());
                    }
                    if !more.is_empty() || !self.ret_more_tys.is_empty() {
                        if hi.is_none() {
                            return Err("return of more than two words missing its second word".into());
                        }
                        if more.len() != self.ret_more_tys.len() {
                            return Err(format!(
                                "return carries {} words past the second, the function {}",
                                more.len(),
                                self.ret_more_tys.len()
                            ));
                        }
                        for (v, rt) in more.iter().zip(&self.ret_more_tys) {
                            if !self.vty(*v).le(*rt) && self.vty(*v) != *rt {
                                return Err(format!("return word {} : {} vs {rt}", v, self.vty(*v)));
                            }
                        }
                    }
                    if let Some(v) = lo
                        && let Some(rt) = self.ret_ty
                            && !self.vty(*v).le(rt) && self.vty(*v) != rt {
                                return Err(format!("return {} : {} vs {rt}", v, self.vty(*v)));
                            }
                    if let Some(v) = hi {
                        if self.ret_layout != MirLayout::TwoSlot {
                            return Err("hi return word requires twoslot layout".into());
                        }
                        if let Some(rt) = self.ret_hi_ty
                            && !self.vty(*v).le(rt) && self.vty(*v) != rt {
                                return Err(format!("return hi {} : {} vs {rt}", v, self.vty(*v)));
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

    /// Every use is dominated by its definition: a φ operand by the end of
    /// the block it comes from, any other use by an earlier instruction of
    /// its block or by a dominating block. Params and blocks nothing reaches
    /// pass. Debug builds run it after each pass (#1869), and any build
    /// under `COIL_MIR_VERIFY`.
    pub fn verify_dominance(&self) -> Result<(), String> {
        let dom = super::licm::Dominators::new(self, &self.preds());
        let mut def: Vec<Option<(BlockId, usize)>> = vec![None; self.types.len()];
        for b in &self.blocks {
            for (i, inst) in b.insts.iter().enumerate() {
                for d in inst.dests() {
                    def[d.index()] = Some((b.id, i));
                }
            }
        }
        let dominated = |v: ValueId, b: BlockId, i: usize| match def.get(v.index()).copied().flatten() {
            None => true,
            Some((db, di)) if db == b => di < i,
            Some((db, _)) => dom.dominates(db, b),
        };
        for b in &self.blocks {
            for (i, inst) in b.insts.iter().enumerate() {
                if let MirInst::Phi { dest, args, .. } = inst {
                    if let Some((from, v)) = args.iter().find(|(from, v)| !dominated(*v, *from, usize::MAX)) {
                        return Err(format!("{dest} phi reads {v} from {from}, which its def does not dominate"));
                    }
                } else if let Some(o) = inst.operands().into_iter().find(|&o| !dominated(o, b.id, i)) {
                    return Err(format!("{} in {} reads {o} before its def dominates it", inst.dest(), b.id));
                }
            }
            if let Some(term) = &b.term {
                let mut uses = Vec::new();
                term.clone().rewrite_values(|v| {
                    uses.push(v);
                    v
                });
                if let Some(o) = uses.into_iter().find(|&o| !dominated(o, b.id, usize::MAX)) {
                    return Err(format!("{} terminator reads {o} before its def dominates it", b.id));
                }
            }
        }
        Ok(())
    }

    /// A lane op's element words past two (`more`, `n` of them): at most
    /// [`crate::mir::MAX_RET_MORE`], and when there are any, every word of
    /// the element a number and its shape a plain tuple's (no tags).
    fn check_wide_lane<'a>(&self, at: ValueId, words: impl Iterator<Item = &'a ValueId>, n: usize, shape: Option<&super::MirLaneShape>) -> Result<(), String> {
        if n > crate::mir::MAX_RET_MORE {
            return Err(format!("{at} lane element of {} words", 2 + n));
        }
        if n == 0 {
            return Ok(());
        }
        if shape.is_some_and(|s| *s != super::MirLaneShape::default()) {
            return Err(format!("{at} wide lane element with a tagged shape"));
        }
        if let Some(w) = words.copied().find(|w| !matches!(self.vty(*w), MirTy::I64 | MirTy::F64 | MirTy::Bool)) {
            return Err(format!("{w} wide lane word is not a number"));
        }
        Ok(())
    }

    fn check_inst(&self, inst: &MirInst, preds: &[BlockId]) -> Result<(), String> {
        match inst {
            MirInst::Const { dest, c } => {
                if self.vty(*dest) != c.ty() {
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
                let lt = self.vty(*lhs);
                let rt = self.vty(*rhs);
                // `word & 1` reads a niche `Result`'s tag as an `i64`.
                let heap_bit = (ty.is_heap_word() || lt.is_heap_word() || rt.is_heap_word())
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
                } else if *ty == MirTy::Bool {
                    let logic = matches!(
                        op,
                        super::inst::MirBinOp::BitAnd | super::inst::MirBinOp::BitOr
                    );
                    if !logic || lt != MirTy::Bool || rt != MirTy::Bool {
                        return Err(format!("{dest} binop on {ty}"));
                    }
                } else {
                    if !ty.is_numeric() {
                        return Err(format!("{dest} binop on {ty}"));
                    }
                    if op.requires_int() && !ty.is_int() {
                        return Err(format!("{dest} bitwise on {ty}"));
                    }
                    if !op.takes(*ty) {
                        return Err(format!("{dest} pow on {ty}"));
                    }
                    if lt != ty.word_class() || rt != ty.word_class() {
                        return Err(format!("{dest} binop operand type"));
                    }
                }
                if self.vty(*dest) != ty.word_class() {
                    return Err(format!("{dest} binop dest type"));
                }
            }
            MirInst::Cmp {
                dest, ty, lhs, rhs, ..
            } => {
                let lt = self.vty(*lhs);
                let rt = self.vty(*rhs);
                if ty.is_heap_word() {
                    let ok = |t: MirTy| t.is_heap_word() || t == MirTy::I64;
                    if !ok(lt) || !ok(rt) {
                        return Err(format!("{dest} heap cmp operand type"));
                    }
                } else {
                    if !ty.is_numeric() || *ty == MirTy::Bool {
                        return Err(format!("{dest} cmp on {ty}"));
                    }
                    if lt != ty.word_class() || rt != ty.word_class() {
                        return Err(format!("{dest} cmp operand type"));
                    }
                }
                if self.vty(*dest) != MirTy::Bool {
                    return Err(format!("{dest} cmp dest is not bool"));
                }
            }
            MirInst::Unary { dest, op, src } => match op {
                super::inst::MirUnaryOp::Not => {
                    let t = self.vty(*src);
                    if self.vty(*dest) != MirTy::Bool {
                        return Err(format!("{dest} lnot dest is not bool"));
                    }
                    if t != MirTy::Bool && !t.is_int() && !t.is_heap_word() {
                        return Err(format!("{dest} lnot on {t}"));
                    }
                }
                super::inst::MirUnaryOp::Neg => {
                    let t = self.vty(*src);
                    if !t.is_int() && !t.is_float() {
                        return Err(format!("{dest} neg on {t}"));
                    }
                    if self.vty(*dest) != t {
                        return Err(format!("{dest} neg dest type"));
                    }
                }
            },
            MirInst::VecLoop { dest, pos, from, to, kind } => {
                if [*from, *to].iter().chain(pos.iter()).any(|&v| self.vty(v) != MirTy::I64) {
                    return Err(format!("{dest} vec loop range is not i64"));
                }
                let want = match kind {
                    super::inst::VecLoopKind::Reduce { init, .. } => self.vty(*init),
                    super::inst::VecLoopKind::Store { .. } => MirTy::I64,
                    super::inst::VecLoopKind::Push { out, .. } => self.vty(*out),
                };
                if self.vty(*dest) != want {
                    return Err(format!("{dest} vec loop dest type"));
                }
                if pos.is_some() != kind.uses_index() {
                    return Err(format!("{dest} vec loop position without an index, or none for one"));
                }
            }
            MirInst::ByteScan { dest, from, bound, streams, scalars, .. } => {
                if [*dest, *from, *bound].iter().any(|&v| self.vty(v) != MirTy::I64) {
                    return Err(format!("{dest} byte scan position is not i64"));
                }
                if streams.iter().any(|&(a, i)| !self.vty(a).is_heap_word() || self.vty(i) != MirTy::I64)
                    || scalars.iter().any(|&k| self.vty(k) != MirTy::I64)
                {
                    return Err(format!("{dest} byte scan operand type"));
                }
            }
            MirInst::Cast {
                dest,
                kind,
                to,
                src,
            } => {
                let from = self.ty(*src);
                if !kind.allows(from, *to) || self.ty(*dest) != *to {
                    return Err(format!("{dest} illegal cast {from} -> {to}"));
                }
            }
            MirInst::HostInvoke {
                dest,
                dest_hi,
                native_id,
                args,
                ..
            } => {
                if let Some(hi) = dest_hi
                    && self.vty(*hi) != MirTy::I64
                {
                    return Err(format!("{hi} host tag is not i64"));
                }
                let Some(spec) = super::host_allow::host_edge_spec(*native_id) else {
                    return Err(format!("{dest} host {native_id} not a typed edge"));
                };
                if !super::host_allow::host_arity_ok(*native_id, spec.args.len(), args.len()) {
                    return Err(format!("{dest} host arity"));
                }
                for (i, (a, ty)) in args.iter().zip(spec.args.iter()).enumerate() {
                    if !super::host_allow::host_arg_ok(*native_id, *ty, self.vty(*a)) {
                        return Err(format!("{dest} host arg {i} type"));
                    }
                }
                let payload = dest_hi.is_some() && self.vty(*dest).is_word_lane();
                if !payload && !super::host_allow::host_result_ok(*native_id, spec.ret, self.vty(*dest)) {
                    return Err(format!("{dest} host dest type"));
                }
            }
            MirInst::Ffi { dest, args, .. } => {
                if self.vty(*dest) != MirTy::HeapRef {
                    return Err(format!("{dest} ffi dest is not heapref"));
                }
                if args.iter().any(|a| !self.vty(*a).is_word_lane()) {
                    return Err(format!("{dest} ffi arg is not a word lane"));
                }
            }
            MirInst::ExternCall { dest, sig, args } => {
                if args.len() != 3 + sig.variadic.as_ref().map_or(sig.params.len(), Vec::len) {
                    return Err(format!("{dest} extern call arity"));
                }
                if args.iter().chain([dest]).any(|a| !self.vty(*a).is_word_lane()) {
                    return Err(format!("{dest} extern call word is not a word lane"));
                }
            }
            MirInst::CallIndirect { dest, callee, args, dicts, pair } => {
                if *dicts as usize > args.len() {
                    return Err(format!("{dest} CallIndirect dicts exceed args"));
                }
                if let Some((hi, _)) = pair {
                    if *dicts != 0 {
                        return Err(format!("{dest} CallIndirect pair with dictionaries"));
                    }
                    if !self.vty(*hi).is_word_lane() {
                        return Err(format!("{dest} CallIndirect second word is not a word lane"));
                    }
                }
                if !self.vty(*dest).is_word_lane() {
                    return Err(format!("{dest} CallIndirect dest is not a word lane"));
                }
                if !matches!(self.vty(*callee), MirTy::HeapRef | MirTy::I64) {
                    return Err(format!("{dest} CallIndirect callee is not heapref or a code pointer"));
                }
                if args.iter().any(|a| !self.vty(*a).is_word_lane()) {
                    return Err(format!("{dest} CallIndirect arg is not a word lane"));
                }
            }
            MirInst::Call {
                dest,
                dest_hi,
                dest_more,
                args,
                ..
            } => {
                if !self.vty(*dest).is_word_lane() {
                    return Err(format!("{dest} call dest is not a word lane"));
                }
                if let Some(hi) = dest_hi
                    && !self.vty(*hi).is_word_lane() {
                        return Err(format!("{hi} call hi dest is not a word lane"));
                    }
                if !dest_more.is_empty() && dest_hi.is_none() {
                    return Err(format!("{dest} call has more result words but no second"));
                }
                if dest_more.len() > crate::mir::MAX_CALL_RET_MORE {
                    return Err(format!("{dest} call returns {} words", 2 + dest_more.len()));
                }
                if let Some(w) = dest_more.iter().find(|w| !self.vty(**w).is_word_lane()) {
                    return Err(format!("{w} call result word is not a word lane"));
                }
                for (i, a) in args.iter().enumerate() {
                    if !self.vty(*a).is_word_lane() {
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
                if self.vty(*array) != MirTy::HeapRef {
                    return Err(format!("{dest} Index array is not heapref"));
                }
                if !self.vty(*index).is_int() {
                    return Err(format!("{dest} Index index type"));
                }
                if !self.vty(*dest).is_word_lane() {
                    return Err(format!("{dest} Index dest type"));
                }
            }
            MirInst::StoreIndex {
                dest,
                array,
                index,
                value,
                scalar,
                ..
            } => {
                if *scalar && !matches!(self.vty(*value), MirTy::I64 | MirTy::F64 | MirTy::Bool) {
                    return Err(format!("{dest} StoreIndex of a number element stores an object word"));
                }
                if self.vty(*array) != MirTy::HeapRef {
                    return Err(format!("{dest} StoreIndex array is not heapref"));
                }
                if !self.vty(*index).is_int() {
                    return Err(format!("{dest} StoreIndex index type"));
                }
                if !self.vty(*value).is_word_lane() || self.vty(*dest) != self.vty(*value) {
                    return Err(format!("{dest} StoreIndex value type"));
                }
            }
            MirInst::LaneIndex { dest, dest_hi, dest_more, array, index } => {
                if self.vty(*array) != MirTy::HeapRef || !self.vty(*index).is_int() {
                    return Err(format!("{dest} LaneIndex operand type"));
                }
                if !self.vty(*dest).is_word_lane() || !self.vty(*dest_hi).is_word_lane() {
                    return Err(format!("{dest} LaneIndex dest type"));
                }
                self.check_wide_lane(*dest, [*dest, *dest_hi].iter().chain(dest_more), dest_more.len(), None)?;
            }
            MirInst::LaneStore { dest, array, index, lo, hi, more, shape } => {
                if self.vty(*array) != MirTy::HeapRef || !self.vty(*index).is_int() {
                    return Err(format!("{dest} LaneStore operand type"));
                }
                if !self.vty(*lo).is_word_lane() || !self.vty(*hi).is_word_lane() || self.vty(*dest) != MirTy::Bool {
                    return Err(format!("{dest} LaneStore value type"));
                }
                self.check_wide_lane(*dest, [*lo, *hi].iter().chain(more), more.len(), Some(shape))?;
            }
            MirInst::LanesToWords { dest, array, .. } => {
                if self.vty(*array) != MirTy::HeapRef || self.vty(*dest) != MirTy::HeapRef {
                    return Err(format!("{dest} LanesToWords array type"));
                }
            }
            MirInst::LanePush { dest, array, lo, hi, more, shape } => {
                if self.vty(*array) != MirTy::HeapRef || self.vty(*dest) != MirTy::HeapRef {
                    return Err(format!("{dest} LanePush array type"));
                }
                if !self.vty(*lo).is_word_lane() || !self.vty(*hi).is_word_lane() {
                    return Err(format!("{dest} LanePush value type"));
                }
                self.check_wide_lane(*dest, [*lo, *hi].iter().chain(more), more.len(), Some(shape))?;
            }
            MirInst::ArrayLen { dest, array } => {
                if self.vty(*array) != MirTy::HeapRef {
                    return Err(format!("{dest} ArrayLen array is not heapref"));
                }
                if self.vty(*dest) != MirTy::I64 {
                    return Err(format!("{dest} ArrayLen dest type"));
                }
            }
            MirInst::ResumeCoro { dest, handle, send } => {
                if self.vty(*handle) != MirTy::HeapRef {
                    return Err(format!("{dest} ResumeCoro handle is not heapref"));
                }
                if !self.vty(*dest).is_word_lane() {
                    return Err(format!("{dest} ResumeCoro dest type"));
                }
                if let Some(s) = send
                    && !self.vty(*s).is_word_lane() {
                        return Err(format!("{dest} ResumeCoro send type"));
                    }
            }
            MirInst::Yield { dest, value, from, receive } => {
                if !self.vty(*value).is_word_lane() || (*from && self.vty(*value) != MirTy::HeapRef) {
                    return Err(format!("{dest} Yield value type"));
                }
                if (*from && *receive) || (!*receive && self.vty(*dest) != MirTy::Bool) || !self.vty(*dest).is_word_lane() {
                    return Err(format!("{dest} Yield dest type"));
                }
            }
            MirInst::DoneCoro { dest, handle } => {
                if self.vty(*handle) != MirTy::HeapRef {
                    return Err(format!("{dest} DoneCoro handle is not heapref"));
                }
                if self.vty(*dest) != MirTy::Bool {
                    return Err(format!("{dest} DoneCoro dest type"));
                }
            }
            MirInst::LoadStatic { dest, .. } => {
                if !self.vty(*dest).is_word_lane() {
                    return Err(format!("{dest} LoadStatic dest type"));
                }
            }
            MirInst::HomeLoad { dest, home } => {
                if self.homes.get(usize::from(*home)) != Some(&self.vty(*dest)) {
                    return Err(format!("{dest} HomeLoad type"));
                }
            }
            MirInst::HomeStore { dest, home, value } => {
                if self.vty(*dest) != MirTy::Bool {
                    return Err(format!("{dest} HomeStore dest is not bool"));
                }
                let (have, want) = (self.vty(*value), self.homes.get(usize::from(*home)).copied().unwrap_or(MirTy::Bottom));
                if !have.is_word_lane() || have.is_heap_word() != want.is_heap_word() {
                    return Err(format!("{dest} HomeStore value type"));
                }
            }
            MirInst::StoreStatic { dest, value, .. } => {
                if self.vty(*dest) != MirTy::Bool {
                    return Err(format!("{dest} StoreStatic dest is not bool"));
                }
                if !self.vty(*value).is_word_lane() {
                    return Err(format!("{dest} StoreStatic value type"));
                }
            }
            MirInst::Probe { dest, .. } => {
                if self.vty(*dest) != MirTy::Bool {
                    return Err(format!("{dest} Probe dest is not bool"));
                }
            }
            MirInst::ClearWord { dest, value } | MirInst::KeepAlive { dest, value } => {
                if self.vty(*dest) != MirTy::Bool {
                    return Err(format!("{dest} ClearWord dest is not bool"));
                }
                if !self.vty(*value).is_word_lane() {
                    return Err(format!("{dest} ClearWord value type"));
                }
            }
            MirInst::ArrayPush {
                dest,
                array,
                value,
            } => {
                if self.vty(*array) != MirTy::HeapRef || self.vty(*dest) != MirTy::HeapRef {
                    return Err(format!("{dest} ArrayPush array type"));
                }
                if !self.vty(*value).is_word_lane() {
                    return Err(format!("{dest} ArrayPush value type"));
                }
            }
            MirInst::MatchPayload {
                dest,
                scrutinee,
                index: _,
            } => {
                if !self.vty(*scrutinee).is_specialized() {
                    return Err(format!("{dest} MatchPayload scrutinee type"));
                }
                if !self.vty(*dest).is_specialized() {
                    return Err(format!("{dest} MatchPayload dest type"));
                }
            }
            MirInst::ProductLoad {
                dest,
                object,
                // Any slot slot: the dense form packs one that fits a
                // byte, and the stack form takes a full word (#1498).
                slot: _,
            } => {
                if self.vty(*object) != MirTy::HeapRef {
                    return Err(format!("{dest} ProductLoad object type"));
                }
                if !self.vty(*dest).is_specialized() {
                    return Err(format!("{dest} ProductLoad dest type"));
                }
            }
            MirInst::HeapFieldStore {
                dest,
                object,
                value,
                // Any slot index: the dense form packs one that fits a
                // byte, and the stack form takes a full word (#1498).
                index: _,
                scalar,
            } => {
                if self.vty(*object) != MirTy::HeapRef {
                    return Err(format!("{dest} HeapFieldStore object type"));
                }
                if *scalar && !matches!(self.vty(*value), MirTy::I64 | MirTy::F64 | MirTy::Bool) {
                    return Err(format!("{dest} HeapFieldStore of a number field stores an object word"));
                }
                if !self.vty(*value).is_specialized() {
                    return Err(format!("{dest} HeapFieldStore value type"));
                }
                if self.vty(*dest) != self.vty(*value) {
                    return Err(format!("{dest} HeapFieldStore dest type"));
                }
            }
            MirInst::Alloc { dest, elems, kind, .. } => {
                if self.vty(*dest) != MirTy::HeapRef {
                    return Err(format!("{dest} Alloc dest is not heapref"));
                }
                if let super::MirAllocKind::Lanes { tag_mask, more } = *kind {
                    let k = 2 + usize::from(more);
                    if usize::from(more) > crate::mir::MAX_RET_MORE || !elems.len().is_multiple_of(k) || (more != 0 && tag_mask != 0) {
                        return Err(format!("{dest} lane Alloc of {} words, {k} per element", elems.len()));
                    }
                }
                for (i, e) in elems.iter().enumerate() {
                    if !self.vty(*e).is_specialized() {
                        return Err(format!("{dest} Alloc elem {i} type"));
                    }
                }
            }
            MirInst::AllocAt { dest, slot, elems, .. } => {
                if self.vty(*dest) != MirTy::HeapRef || slot.is_some_and(|s| self.vty(s) != MirTy::I64) {
                    return Err(format!("{dest} AllocAt type"));
                }
                for (i, e) in elems.iter().enumerate() {
                    if !self.vty(*e).is_specialized() {
                        return Err(format!("{dest} AllocAt elem {i} type"));
                    }
                }
            }
            MirInst::RcReuse { dest, src, .. } => {
                if self.vty(*dest) != MirTy::I64 || !matches!(self.vty(*src), MirTy::HeapRef | MirTy::Value) {
                    return Err(format!("{dest} RcReuse type"));
                }
            }
            MirInst::GcBarrier { dest, object, .. } => {
                if self.vty(*dest) != MirTy::HeapRef {
                    return Err(format!("{dest} GcBarrier dest is not heapref"));
                }
                if !self.vty(*object).is_heap_word() {
                    return Err(format!("{dest} GcBarrier object is not a heap word"));
                }
            }
            MirInst::String { dest, .. } => {
                if self.vty(*dest) != MirTy::HeapRef {
                    return Err(format!("{dest} String dest is not heapref"));
                }
            }
            MirInst::CodePtr { dest, .. } => {
                if self.vty(*dest) != MirTy::I64 {
                    return Err(format!("{dest} CodePtr dest is not i64"));
                }
            }
            MirInst::FreeEarly { dest, value, .. } => {
                if self.vty(*dest) != MirTy::Bool {
                    return Err(format!("{dest} FreeEarly dest is not bool"));
                }
                if !matches!(self.vty(*value), MirTy::HeapRef | MirTy::NicheOpt | MirTy::NicheRes | MirTy::I64) {
                    return Err(format!("{dest} FreeEarly value type"));
                }
            }
            MirInst::Print { dest, src } => {
                if self.vty(*dest) != MirTy::Bool {
                    return Err(format!("{dest} Print dest is not bool"));
                }
                if !self.vty(*src).is_specialized() {
                    return Err(format!("{dest} Print src type"));
                }
            }
            MirInst::Panic { dest, msg } => {
                if self.vty(*dest) != MirTy::Bool {
                    return Err(format!("{dest} Panic dest is not bool"));
                }
                if self.vty(*msg) != MirTy::HeapRef {
                    return Err(format!("{dest} Panic msg is not heapref"));
                }
            }
            MirInst::Format { dest, fmt, args } => {
                if self.vty(*dest) != MirTy::HeapRef {
                    return Err(format!("{dest} Format dest is not heapref"));
                }
                if self.vty(*fmt) != MirTy::HeapRef && self.vty(*fmt) != MirTy::Value {
                    return Err(format!("{dest} Format fmt type"));
                }
                for (i, a) in args.iter().enumerate() {
                    if !self.vty(*a).is_specialized() {
                        return Err(format!("{dest} Format arg {i} type"));
                    }
                }
            }
            MirInst::Cow { dest, src, .. } => {
                if self.vty(*dest) != MirTy::HeapRef || self.vty(*src) != MirTy::HeapRef {
                    return Err(format!("{dest} Cow type"));
                }
            }
            MirInst::CowIndex { dest, array, index } => {
                if self.vty(*dest) != MirTy::HeapRef || self.vty(*array) != MirTy::HeapRef || !self.vty(*index).is_int() {
                    return Err(format!("{dest} CowIndex type"));
                }
            }
            MirInst::CowPin { dest, src } => {
                if self.vty(*dest) != MirTy::Bool || self.vty(*src) != MirTy::HeapRef {
                    return Err(format!("{dest} CowPin type"));
                }
            }
            MirInst::CowUnpin { dest, src, saved } => {
                if self.vty(*dest) != MirTy::Bool || self.vty(*src) != MirTy::HeapRef || self.vty(*saved) != MirTy::Bool {
                    return Err(format!("{dest} CowUnpin type"));
                }
            }
            MirInst::RcInc { dest, src, guard } | MirInst::RcDec { dest, src, guard } => {
                if self.vty(*dest) != MirTy::Bool || !matches!(self.vty(*src), MirTy::HeapRef | MirTy::NicheOpt | MirTy::NicheRes | MirTy::I64 | MirTy::Value) {
                    return Err(format!("{dest} reference count op type"));
                }
                if guard.is_some_and(|g| self.vty(g) != MirTy::Bool) {
                    return Err(format!("{dest} reference count op guard is not bool"));
                }
            }
            MirInst::Stringify { dest, src, .. } => {
                if self.vty(*dest) != MirTy::HeapRef {
                    return Err(format!("{dest} Stringify dest is not heapref"));
                }
                if !self.vty(*src).is_specialized() {
                    return Err(format!("{dest} Stringify src type"));
                }
            }
            MirInst::StrEq { dest, lhs, rhs, .. } => {
                if self.vty(*dest) != MirTy::Bool {
                    return Err(format!("{dest} StrEq dest is not bool"));
                }
                if self.vty(*lhs) != MirTy::HeapRef || self.vty(*rhs) != MirTy::HeapRef {
                    return Err(format!("{dest} StrEq operand type"));
                }
            }
            MirInst::Phi { dest, ty, args } => {
                if self.vty(*dest) != ty.word_class() {
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
                    if self.vty(*v) != ty.word_class() {
                        return Err(format!("{dest} phi arg {v} type"));
                    }
                }
            }
        }
        Ok(())
    }
}

/// `inst` is a planned tail call ([`MirFunc::tail_targets`]) whose result
/// `block` returns. Any other call keeps the caller's frame, which a failed
/// `requires` in the callee blames and a backtrace names.
pub fn is_tail_call_inst(func: &MirFunc, block: &MirBlock, inst: &MirInst) -> bool {
    let MirInst::Call { dest, dest_hi, dest_more, target, .. } = inst else {
        return false;
    };
    if !func.tail_targets.contains(target) {
        return false;
    }
    match &block.term {
        Some(Terminator::Return { lo: Some(v), hi, more }) if v == dest && hi == dest_hi && more == dest_more => {
            matches!(
                block.insts.last(),
                Some(MirInst::Call { dest: d, dest_hi: h, dest_more: m, .. }) if d == dest && h == dest_hi && m == dest_more
            )
        }
        _ => false,
    }
}
