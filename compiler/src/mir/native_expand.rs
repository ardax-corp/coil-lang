//! Vec natives native code runs as MIR.
//!
//! The runtime keeps `vec_insert` and `vec_remove` in the runtime library. In a captured
//! native body [`apply`] writes each `HostInvoke` of them out as the loop it
//! stands for, over the MIR array ops native code already compiles
//! (`ArrayLen`, `Index`, `StoreIndex`, `ArrayPush`). The element moves are
//! word copies, so one expansion serves every element type: a word whose
//! type the call does not say is moved as an `i64`, with no safepoint
//! between its load and its store (a push roots the word it pushes). The
//! element `insert` adds and `remove` takes out is the exception: it stays
//! in a value across the shifting loop, whose header is a safepoint, and
//! the array no longer holds it. As an `i64` it may be an object, so it is
//! listed in [`MirFunc::heap_pair_words`].
//!
//! What stays a runtime call: `vec_from_array`, the copy primitive (it
//! keeps the source's representation, a packed byte or float array stays
//! packed, which the MIR array ops cannot ask for); `vec_pop`, the shrink
//! primitive (`remove` rotates the element to the end and pops it, which
//! also packs the `Option` in the call's layout); and `vec_insert` itself
//! on the out-of-range path only, so its panic is the host native's, as on
//! the runtime (its message alone, no Coil frames). A call whose operand or
//! result types the expansion does not cover stays a `HostInvoke`.

use super::inst::{MirAllocKind, MirBinOp, MirCastKind, MirCmpOp, MirConst};
use super::{BlockId, MirBlock, MirFunc, MirInst, MirTy, Terminator, ValueId};

#[derive(Clone, Copy)]
enum VecNative {
    Insert,
    Remove,
}

struct Ids {
    insert: u16,
    remove: u16,
    pop: u16,
}

impl Ids {
    fn get() -> Self {
        let id = |name: &str| common::host_native_id(name).unwrap_or_else(|| panic!("unknown host native `{name}`")) as u16;
        Self { insert: id("vec_insert"), remove: id("vec_remove"), pop: id("vec_pop") }
    }

    fn which(&self, native_id: u16) -> Option<VecNative> {
        match native_id {
            n if n == self.insert => Some(VecNative::Insert),
            n if n == self.remove => Some(VecNative::Remove),
            _ => None,
        }
    }
}

/// Expand each `HostInvoke` of `vec_insert` / `vec_remove` in `func`.
pub fn apply(func: &mut MirFunc) {
    let ids = Ids::get();
    // A call an expansion writes (out-of-range `insert`) defines a value
    // past the body's own: it stays a call.
    let own = func.types.len();
    let mut b = 0;
    while b < func.blocks.len() {
        let found = func.blocks[b].insts.iter().enumerate().find_map(|(k, inst)| match inst {
            // A call the block ends `unreachable` after only panics (lane
            // code's out-of-range `insert`): it stays a call.
            MirInst::HostInvoke { .. } if k + 1 == func.blocks[b].insts.len() && matches!(func.blocks[b].term, Some(Terminator::Unreachable)) => None,
            MirInst::HostInvoke { dest, dest_hi: None, native_id, layout, args } if dest.index() < own => {
                let which = ids.which(*native_id)?;
                expandable(func, which, *dest, *layout, args).then_some((k, which))
            }
            _ => None,
        });
        match found {
            Some((k, which)) => expand(func, BlockId(b as u32), k, which, &ids),
            None => b += 1,
        }
    }
}

/// Whether the call's operand and result types are ones the expansion
/// writes code for.
fn expandable(func: &MirFunc, which: VecNative, dest: ValueId, layout: u8, args: &[ValueId]) -> bool {
    let arg = |i: usize| args.get(i).map(|v| func.ty(*v));
    let heap = |t: Option<MirTy>| t == Some(MirTy::HeapRef);
    let int = |t: Option<MirTy>| t == Some(MirTy::I64);
    match which {
        VecNative::Insert => {
            args.len() == 3
                && heap(arg(0))
                && int(arg(1))
                && arg(2).is_some_and(MirTy::is_specialized)
                && matches!(func.ty(dest), MirTy::I64 | MirTy::Bool)
        }
        VecNative::Remove => {
            args.len() == 2
                && heap(arg(0))
                && int(arg(1))
                && matches!(
                    (u32::from(layout), func.ty(dest)),
                    (common::HOST_ENUM_LAYOUT_BOXED, MirTy::HeapRef) | (common::HOST_ENUM_LAYOUT_OPTION_NICHE, MirTy::NicheOpt)
                )
        }
    }
}

struct Ex<'a> {
    f: &'a mut MirFunc,
    /// The call's source position, for every value the expansion makes.
    at: ValueId,
}

impl Ex<'_> {
    fn value(&mut self, ty: MirTy) -> ValueId {
        let v = ValueId(self.f.types.len() as u32);
        self.f.types.push(ty);
        if let Some(loc) = self.f.value_locs.get(&self.at).cloned() {
            self.f.value_locs.insert(v, loc);
        }
        if let Some(loc) = self.f.op_locs.get(&self.at).cloned() {
            self.f.op_locs.insert(v, loc);
        }
        v
    }

    fn block(&mut self) -> BlockId {
        let id = BlockId(self.f.blocks.len() as u32);
        self.f.blocks.push(MirBlock::new(id));
        id
    }

    fn push(&mut self, b: BlockId, inst: MirInst) {
        self.f.blocks[b.index()].insts.push(inst);
    }

    fn end(&mut self, b: BlockId, term: Terminator) {
        if let Some(loc) = self.f.value_locs.get(&self.at).cloned() {
            self.f.term_locs.insert(b, loc);
        }
        self.f.blocks[b.index()].term = Some(term);
    }

    fn int(&mut self, b: BlockId, n: i64) -> ValueId {
        let dest = self.value(MirTy::I64);
        self.push(b, MirInst::Const { dest, c: MirConst::I64(n) });
        dest
    }

    fn cmp(&mut self, b: BlockId, op: MirCmpOp, lhs: ValueId, rhs: ValueId) -> ValueId {
        let dest = self.value(MirTy::Bool);
        self.push(b, MirInst::Cmp { dest, op, ty: MirTy::I64, lhs, rhs });
        dest
    }

    fn bin(&mut self, b: BlockId, op: MirBinOp, lhs: ValueId, rhs: ValueId) -> ValueId {
        let dest = self.value(MirTy::I64);
        self.push(b, MirInst::Bin { dest, op, ty: MirTy::I64, lhs, rhs });
        dest
    }

    fn len(&mut self, b: BlockId, array: ValueId) -> ValueId {
        let dest = self.value(MirTy::I64);
        self.push(b, MirInst::ArrayLen { dest, array });
        dest
    }

    fn index(&mut self, b: BlockId, array: ValueId, index: ValueId, ty: MirTy) -> ValueId {
        let dest = self.value(ty);
        self.push(b, MirInst::Index { dest, array, index, unchecked: false });
        dest
    }

    fn store(&mut self, b: BlockId, array: ValueId, index: ValueId, value: ValueId) {
        let dest = self.value(self.f.ty(value));
        self.push(b, MirInst::StoreIndex { dest, array, index, value, unchecked: false });
    }

    fn br(&mut self, b: BlockId, cond: ValueId, taken: BlockId, not_taken: BlockId) {
        self.end(b, Terminator::Br { cond, taken, not_taken });
    }

    fn jump(&mut self, b: BlockId, dest: BlockId) {
        self.end(b, Terminator::Jump { dest });
    }
}

/// Split block `b` at its instruction `k` (the call) and write the call
/// out between the two halves.
fn expand(func: &mut MirFunc, b: BlockId, k: usize, which: VecNative, ids: &Ids) {
    let block = &mut func.blocks[b.index()];
    let MirInst::HostInvoke { dest, layout, args, .. } = block.insts[k].clone() else {
        unreachable!("expand at a HostInvoke");
    };
    let tail: Vec<MirInst> = block.insts.split_off(k + 1);
    block.insts.truncate(k);
    let term = block.term.take();
    let succs = term.as_ref().map(Terminator::succs).unwrap_or_default();
    let mut ex = Ex { f: func, at: dest };
    let rest = ex.block();
    ex.f.blocks[rest.index()].insts = tail;
    ex.f.blocks[rest.index()].term = term;
    if let Some(loc) = ex.f.term_locs.remove(&b) {
        ex.f.term_locs.insert(rest, loc);
    }
    // The successors of the old block now come from `rest`.
    for s in succs {
        for inst in &mut ex.f.blocks[s.index()].insts {
            if let MirInst::Phi { args, .. } = inst {
                for (from, _) in args.iter_mut() {
                    if *from == b {
                        *from = rest;
                    }
                }
            }
        }
    }
    match which {
        VecNative::Insert => insert(&mut ex, b, rest, dest, layout, &args, ids.insert),
        VecNative::Remove => remove(&mut ex, b, rest, dest, layout, &args, ids.pop),
    }
}

/// `v.insert(i, x)`: unless `0 <= i <= len` the runtime's `vec_insert`
/// panics; else push `x`, shift `v[i..len]` up one, store `x` at `i`.
fn insert(ex: &mut Ex, b: BlockId, rest: BlockId, dest: ValueId, layout: u8, args: &[ValueId], native: u16) {
    let (v, i, x) = (args[0], args[1], args[2]);
    let elem = ex.f.ty(x);
    if elem == MirTy::I64 {
        ex.f.heap_pair_words.insert(x);
    }
    let check_hi = ex.block();
    let oob = ex.block();
    let grow = ex.block();
    let head = ex.block();
    let body = ex.block();
    let done = ex.block();

    let n = ex.len(b, v);
    let zero = ex.int(b, 0);
    let one = ex.int(b, 1);
    let below = ex.cmp(b, MirCmpOp::Lt, i, zero);
    ex.br(b, below, oob, check_hi);

    let above = ex.cmp(check_hi, MirCmpOp::Gt, i, n);
    ex.br(check_hi, above, oob, grow);

    // Out of range: the native panics, as the runtime's call does.
    let failed = ex.value(ex.f.ty(dest));
    ex.push(oob, MirInst::HostInvoke { dest: failed, dest_hi: None, native_id: native, layout, args: args.to_vec() });
    ex.end(oob, Terminator::Unreachable);

    let grown = ex.value(MirTy::HeapRef);
    ex.push(grow, MirInst::ArrayPush { dest: grown, array: v, value: x });
    ex.jump(grow, head);

    // for j = n; j > i; j -= 1 { v[j] = v[j - 1] }
    let j = ex.value(MirTy::I64);
    let next = ex.value(MirTy::I64);
    ex.push(head, MirInst::Phi { dest: j, ty: MirTy::I64, args: vec![(grow, n), (body, next)] });
    let more = ex.cmp(head, MirCmpOp::Gt, j, i);
    ex.br(head, more, body, done);

    ex.push(body, MirInst::Bin { dest: next, op: MirBinOp::Sub, ty: MirTy::I64, lhs: j, rhs: one });
    let w = ex.index(body, grown, next, elem);
    ex.store(body, grown, j, w);
    ex.jump(body, head);

    ex.store(done, grown, i, x);
    let unit = match ex.f.ty(dest) {
        MirTy::Bool => MirConst::Bool(false),
        _ => MirConst::I64(0),
    };
    ex.push(done, MirInst::Const { dest, c: unit });
    ex.jump(done, rest);
}

/// `v.remove(i)`: `None` unless `0 <= i < len`; else rotate `v[i]` to the
/// end and pop it, which returns `Some(v[i])` in the call's layout.
fn remove(ex: &mut Ex, b: BlockId, rest: BlockId, dest: ValueId, layout: u8, args: &[ValueId], pop: u16) {
    let (v, i) = (args[0], args[1]);
    let ty = ex.f.ty(dest);
    let check_hi = ex.block();
    let none = ex.block();
    let take = ex.block();
    let head = ex.block();
    let body = ex.block();
    let out = ex.block();
    let join = ex.block();

    let n = ex.len(b, v);
    let zero = ex.int(b, 0);
    let one = ex.int(b, 1);
    let below = ex.cmp(b, MirCmpOp::Lt, i, zero);
    ex.br(b, below, none, check_hi);

    let above = ex.cmp(check_hi, MirCmpOp::Ge, i, n);
    ex.br(check_hi, above, none, take);

    let nothing = ex.value(ty);
    if u32::from(layout) == common::HOST_ENUM_LAYOUT_BOXED {
        ex.push(none, MirInst::Alloc { dest: nothing, kind: MirAllocKind::Enum { tag: 0 }, elems: Vec::new(), kinds: 0 });
    } else {
        ex.push(none, MirInst::Cast { dest: nothing, kind: MirCastKind::Word, to: ty, src: zero });
    }
    ex.jump(none, join);

    // x = v[i]; for j = i; j < n - 1; j += 1 { v[j] = v[j + 1] }; v[n - 1] = x
    let x = ex.index(take, v, i, MirTy::I64);
    ex.f.heap_pair_words.insert(x);
    let last = ex.bin(take, MirBinOp::Sub, n, one);
    ex.jump(take, head);

    let j = ex.value(MirTy::I64);
    let next = ex.value(MirTy::I64);
    ex.push(head, MirInst::Phi { dest: j, ty: MirTy::I64, args: vec![(take, i), (body, next)] });
    let more = ex.cmp(head, MirCmpOp::Lt, j, last);
    ex.br(head, more, body, out);

    ex.push(body, MirInst::Bin { dest: next, op: MirBinOp::Add, ty: MirTy::I64, lhs: j, rhs: one });
    let w = ex.index(body, v, next, MirTy::I64);
    ex.store(body, v, j, w);
    ex.jump(body, head);

    ex.store(out, v, last, x);
    let some = ex.value(ty);
    ex.push(out, MirInst::HostInvoke { dest: some, dest_hi: None, native_id: pop, layout, args: vec![v] });
    ex.jump(out, join);

    ex.push(join, MirInst::Phi { dest, ty, args: vec![(none, nothing), (out, some)] });
    ex.jump(join, rest);
}

/// Expand the Vec natives in every captured body.
pub fn apply_all<'a>(bodies: impl Iterator<Item = &'a mut MirFunc>) {
    bodies.for_each(apply);
}
