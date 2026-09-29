//! Precise frame maps ([`common::PreciseFrameMap`]) for the interpreter GC.
//!
//! Two sources, both checked against the final bytecode of each body:
//! - heap-free functions (checker types, see `Compiler::fn_is_heap_free`)
//!   get one empty map valid at every PC;
//! - every other body gets a forward "may hold a heap word" dataflow over the
//!   physical words of its frame, recording the complete heap slots after
//!   each allocating / host op and below each `CALL`'s arguments.
//!
//! Any opcode the dataflow does not model, a stack height that disagrees at a
//! join, or control flow entering the body anywhere but its entry leaves the
//! body without a map, so its frame keeps the conservative scan.

use std::collections::{HashMap, HashSet};

use common::{Byte, Instruction, PreciseFrameMap, SlotMap};

/// Frame rows for the bodies in `entries`: precise heap slots when a body can
/// be described, and a frame extent for `needs_extent` bodies (allocating
/// dense code without S2b maps, whose registers may sit past the cursor).
/// Without a decodable extent such a frame is scanned to the end of the stack.
/// Word kinds (`common::WORD_*`) of a function's parameters (entry slots,
/// in order) and of its one-word return, from its checked signature.
#[derive(Clone, Debug, Default)]
pub struct FnWordKinds {
    pub params: Vec<u8>,
    pub ret: u8,
}

#[allow(clippy::too_many_arguments)]
pub fn bind_precise_frames(
    heap_free: &HashSet<String>,
    needs_extent: &HashSet<String>,
    bytecode: &[Byte],
    constants: &[u64],
    match_arities: &HashMap<u32, u32>,
    entries: &[(String, u32)],
    entry_sps: &HashMap<String, u32>,
    prologue_entry: u32,
    fn_kinds: &HashMap<String, FnWordKinds>,
) -> Vec<PreciseFrameMap> {
    // A `CALL` result is a must-pointer when every name bound to the target
    // declares a pointer return.
    let mut ret_ptr: HashMap<u32, bool> = HashMap::new();
    for (name, pc) in entries {
        let ptr = fn_kinds
            .get(name)
            .is_some_and(|k| k.ret == common::WORD_POINTER);
        let e = ret_ptr.entry(*pc).or_insert(ptr);
        *e = *e && ptr;
    }
    let mut starts: Vec<u32> = entries.iter().map(|(_, pc)| *pc).collect();
    starts.sort_unstable();
    starts.dedup();
    let foreign = inbound_targets(bytecode, constants);
    let mut out = Vec::new();
    for (i, &entry) in starts.iter().enumerate() {
        let end = starts.get(i + 1).copied().unwrap_or(bytecode.len() as u32);
        let Some(body) = bytecode.get(entry as usize..end as usize) else {
            continue;
        };
        let names = || entries.iter().filter(|(_, pc)| *pc == entry).map(|(n, _)| n);
        // Only bodies whose registers may sit past the cursor get an extent:
        // widening every conservative frame also roots stale dead words.
        let frame_words = if names().any(|n| needs_extent.contains(n)) {
            frame_extent(body, constants).unwrap_or_else(|| {
                debug_assert!(false, "dense body without a decodable frame extent");
                u32::MAX
            })
        } else {
            0
        };
        let mut row = PreciseFrameMap {
            entry_pc: entry,
            end_pc: end,
            any_pc: None,
            at_pc: Vec::new(),
            frame_words,
        };
        let enters_mid_body = foreign
            .iter()
            .any(|&(from, to)| to > entry && to < end && !(from >= entry && from < end));
        if !enters_mid_body {
            let heap_free_fn = names().any(|n| heap_free.contains(n));
            if heap_free_fn
                && ends_in_exit(body)
                && body.iter().all(|b| !puts_heap_word(*b.bytecode()))
                && closure_entries(bytecode, constants, entry as usize, end as usize)
                    .is_some_and(|c| c.is_empty())
            {
                row.any_pc = Some(Vec::new());
            } else {
                // Words the host pushes when it calls a body no bytecode references.
                let declared = names()
                    .filter_map(|n| entry_sps.get(n).map(|&sp| sp as usize))
                    .max();
                // Parameter kinds, when every name for this body agrees.
                let mut kinds = names().map(|n| fn_kinds.get(n).map(|k| &k.params));
                let first = kinds.next().flatten();
                let params: Vec<bool> = match first {
                    Some(p) if kinds.all(|k| k == Some(p)) => {
                        p.iter().map(|&k| k == common::WORD_POINTER).collect()
                    }
                    _ => Vec::new(),
                };
                let seed = EntrySeed {
                    prologue_entry: prologue_entry as usize,
                    declared,
                    params,
                };
                if let Some(at_pc) = analyze_body(
                    bytecode,
                    constants,
                    match_arities,
                    entry,
                    end,
                    &seed,
                    &ret_ptr,
                ) {
                    row.at_pc = at_pc;
                }
            }
        }
        if row.any_pc.is_some() || !row.at_pc.is_empty() || row.frame_words != 0 {
            out.push(row);
        }
    }
    out
}

/// Highest frame word the body can touch, plus one: `None` when an op is not
/// known to be slot-free and its slot operands are not decoded here.
fn frame_extent(body: &[Byte], constants: &[u64]) -> Option<u32> {
    let mut words = 0usize;
    for b in body {
        words = words.max(slot_extent(b, constants)?);
    }
    u32::try_from(words).ok()
}

/// Frame words op `b` names (highest slot + 1); `Some(0)` for slot-free ops.
/// The two-word dense packs are covered word by word (their tails decode as
/// `DenseBin` / `BinSlot*Jmp*`).
fn slot_extent(b: &Byte, constants: &[u64]) -> Option<usize> {
    use Instruction::*;
    let pool = |i: usize| constants.get(i).copied();
    let one = |s: usize| Some(s + 1);
    let max = |xs: &[usize]| xs.iter().max().map(|m| m + 1);
    match *b.bytecode() {
        LOAD | STORE | StorePop => {
            max(&(0..b.load_store_count()).map(|i| b.load_store_slot_at(i) as usize).collect::<Vec<_>>())
        }
        Seek | LoadReturnSlot => Some(b.operand_u32() as usize),
        BinSlotImm => one(b.bin_slot_imm_parts().1),
        BinSlotSlot => {
            let (_, a, c) = b.bin_slot_slot_parts();
            max(&[a, c])
        }
        BinSlotImmJmpf | BinSlotImmJmpt => one(b.bin_slot_imm_jmpf_parts().1),
        BinSlotSlotJmpf | BinSlotSlotJmpt => {
            let (_, a, pool_idx) = b.bin_slot_slot_jmpf_parts();
            max(&[a, (pool(pool_idx)? as u32 & 0xFF) as usize])
        }
        BinSlotImmStore => {
            let (_, src, pool_idx) = b.bin_slot_imm_store_parts();
            max(&[src, (pool(pool_idx)? >> 32) as usize])
        }
        BinSlotSlotStore => {
            let (_, a, c, dest) = b.bin_slot_slot_store_parts();
            max(&[a, c, dest])
        }
        INC | DEC => one(b.inc_dec_parts().0),
        UnpackAt => {
            let op = b.operand_u32();
            Some((op & 0xFFFF) as usize + (op >> 16) as usize)
        }
        TailCall => Some(b.call_parts().0),
        DenseBin | DenseBin2 | DenseBinJmpf | DenseCmp | DenseIndex | DenseIndexJmpf
        | DenseStoreIndex | DenseFieldLoad | DenseFieldStore | DenseArrayPush => {
            let (_, d, x, y) = b.dense_abc_parts();
            max(&[d, x, y])
        }
        DenseMake => {
            let (_, dest, arity, base) = b.dense_abc_parts();
            max(&[dest, base + arity.max(1) - 1])
        }
        DenseMakeK => {
            let (_, dest, arity, base, _) = b.dense_make_k_parts(constants)?;
            max(&[dest, base + arity.max(1) - 1])
        }
        DenseMakeObject => one(common::dense::unpack_make_object(b.operand_u32()).0 as usize),
        DenseConst => one(b.dense_const_parts().1),
        DenseMove => {
            let (d, s) = b.dense_move_parts();
            max(&[d, s])
        }
        DenseArrayLen => {
            let (d, a) = b.dense_move_parts();
            max(&[d, a])
        }
        DenseUnary | DenseCast => {
            let (_, d, s) = b.dense_unary_parts();
            max(&[d, s])
        }
        DensePush => {
            let (arity, base) = b.dense_move_parts();
            Some(base + arity)
        }
        VLoad | VStore => {
            let (_, _, arr, idx) = b.dense_abc_parts();
            max(&[arr, idx])
        }
        // Splat reads a scalar slot; other operands are vector registers.
        VBin => one(b.dense_abc_parts().2),
        VReduce => one(b.dense_abc_parts().1),
        VMove | VFma => Some(0),
        NOOP | DATA | HALT | Panic | CONST | STRING | CodePtr | DUPLICATE | POP | PRINT
        | ADD | SUB | MUL | DIV | MOD | LE | LEQ | GT | GEQ | EQ | NEQ | Pow | BITAND | BITOR
        | ADDF | SUBF | MULF | DIVF | MODF | LEF | LEQF | GTF | GEQF | PowF | SHL | SHR | XOR
        | AND | OR | NOT | LogNot | NEG | NEGF | CastIntToFloat | CastFloatToInt
        | CastIntToByte | CastByteToInt | CastIntToBool | CastBoolToInt | JMP | JMPF | JMPT
        | CmpJmpf | CmpJmpt | LogNotJmpf | LogNotJmpt | JumpIfMatch | Unpack | RETURN
        | ConstReturnImm | BinReturn | ReturnPair | MakeEnumReturn | CALL | CallIndirect
        | HostInvoke | MakeArray | MakeTuple | MakeEnum | MakeDict | DictEntries | InitTyped
        | INIT | BoxValue | UnboxValue | FORMAT | STRINGIFY | Index | IndexUnchecked
        | StoreIndex | StoreIndexUnchecked | ArrayLen | ArrayPush | GetField | SetField
        | LoadField | ArrayPin | IndexPin | IndexPinUnchecked | StoreIndexPin
        | StoreIndexPinUnchecked | MakeFn | MakePolyFn | MakePolyFnCapture | MakeCoro
        | ResumeCoro | YieldCoro | YieldFromCoro | DoneCoro | LoadStatic | StoreStatic
        | TagEnumType | TagArrayKind | MakeEnumK | MakeEnumReturnK | MakeTupleK => Some(0),
        _ => None,
    }
}

fn ends_in_exit(body: &[Byte]) -> bool {
    body.last().is_some_and(|b| {
        matches!(
            *b.bytecode(),
            Instruction::RETURN
                | Instruction::ConstReturnImm
                | Instruction::LoadReturnSlot
                | Instruction::BinReturn
                | Instruction::TailCall
                | Instruction::JMP
        )
    })
}

/// Ops that can leave a heap word on the executing frame.
fn puts_heap_word(inst: Instruction) -> bool {
    crate::mir::is_alloc_opcode(inst)
        || matches!(
            inst,
            Instruction::BoxValue
                | Instruction::STRING
                | Instruction::MakeFn
                | Instruction::MakePolyFn
                | Instruction::MakePolyFnCapture
                | Instruction::CodePtr
                | Instruction::MakeCoro
                | Instruction::MakeDict
                | Instruction::DictEntries
                | Instruction::FfiLoad
                | Instruction::DeclareFFI
                | Instruction::FfiInvoke
                | Instruction::GetField
                | Instruction::LoadField
                | Instruction::Index
                | Instruction::DenseIndex
                | Instruction::DenseIndexJmpf
                | Instruction::DenseFieldLoad
        )
}

/// Every `(from, to)` control transfer whose target is a code address:
/// calls, code pointers and decodable jumps. Undecodable jumps are ignored
/// here; the dataflow refuses any body that contains one.
fn inbound_targets(bytecode: &[Byte], constants: &[u64]) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    for (pc, b) in bytecode.iter().enumerate() {
        let to = match *b.bytecode() {
            Instruction::CALL | Instruction::TailCall | Instruction::MakeCoro => {
                Some(b.call_parts().1)
            }
            Instruction::CodePtr | Instruction::MakePolyFn | Instruction::MakePolyFnCapture => {
                Some(b.operand_u32() as usize)
            }
            _ => jump_target(b, constants),
        };
        if let Some(to) = to {
            out.push((pc as u32, to as u32));
        }
    }
    out
}

fn jump_target(b: &Byte, constants: &[u64]) -> Option<usize> {
    let pool = |i: usize| constants.get(i).copied();
    match *b.bytecode() {
        Instruction::JMP | Instruction::JMPF | Instruction::JMPT => Some(b.operand_u32() as usize),
        Instruction::CmpJmpf | Instruction::CmpJmpt => {
            let t = b.cmp_jmpf_parts().1;
            if b.cmp_jmpf_is_pool() {
                pool(t).map(|v| v as usize)
            } else {
                Some(t)
            }
        }
        Instruction::LogNotJmpf | Instruction::LogNotJmpt => {
            let t = b.log_not_jmpf_target();
            if b.log_not_jmpf_is_pool() {
                pool(t).map(|v| v as usize)
            } else {
                Some(t)
            }
        }
        Instruction::BinSlotImmJmpf | Instruction::BinSlotImmJmpt => {
            pool(b.bin_slot_imm_jmpf_parts().2).map(|d| (d >> 32) as usize)
        }
        Instruction::BinSlotSlotJmpf | Instruction::BinSlotSlotJmpt => {
            pool(b.bin_slot_slot_jmpf_parts().2).map(|d| (d >> 32) as usize)
        }
        Instruction::JumpIfMatch => pool((b.operand_u32() & 0xFFFF) as usize).map(|t| t as usize),
        _ => None,
    }
}

/// Physical words of one frame, relative to its base. `bits[p]` is true when
/// word `p` may hold a heap reference; words past `bits` are unknown (stale
/// or written by a callee) and count as heap. `slot[p]` marks words last
/// written as a slot (store, dense register, match payload): only those can
/// be live above the cursor. The cursor is only known to be in `lo..=hi`
/// after joins of differing heights; a `Seek` makes it exact.
#[derive(Clone, PartialEq, Eq)]
struct FrameState {
    lo: usize,
    hi: usize,
    bits: Vec<bool>,
    slot: Vec<bool>,
    /// The word definitely holds a heap pointer (or `0`): every reaching
    /// definition is an allocation or a copy of one. Recorded slots with it
    /// carry `common::PRECISE_SLOT_MUST`; a moving collector may rewrite
    /// them. Missing = no.
    ptr: Vec<bool>,
    /// Must-pointer bits of the top operands (TOS last), tracked relative to
    /// the cursor so a push / pop pair keeps its kind even when the cursor
    /// is inexact. Cleared when the cursor jumps or a slot write may alias
    /// an operand.
    ops: Vec<bool>,
}

/// Words the analysis refuses to track (keeps `u16` slots in range).
const MAX_WORDS: usize = 4096;

impl FrameState {
    fn bit(&self, p: usize) -> bool {
        self.bits.get(p).copied().unwrap_or(true)
    }

    fn must_ptr(&self, p: usize) -> bool {
        self.ptr.get(p).copied().unwrap_or(false)
    }

    fn set_must(&mut self, p: usize, must: bool) {
        if self.ptr.len() <= p {
            if !must {
                return;
            }
            self.ptr.resize(p + 1, false);
        }
        self.ptr[p] = must;
    }

    /// Slot write of an allocation result (dense `DenseMake*`).
    fn set_ptr(&mut self, p: usize) -> Option<()> {
        self.set_copy(p, true, true)
    }

    /// Slot write that copies another word's heap / pointer state.
    fn set_copy(&mut self, p: usize, heap: bool, must: bool) -> Option<()> {
        self.set(p, heap)?;
        self.set_must(p, must);
        Some(())
    }

    /// Push of an allocation result.
    fn push_ptr(&mut self) -> Option<()> {
        self.push_copy(true, true)
    }

    /// Push that copies a word's heap / pointer state; an inexact cursor
    /// loses the pointer guarantee.
    fn push_copy(&mut self, heap: bool, must: bool) -> Option<()> {
        let (lo, hi) = (self.lo, self.hi);
        // An inexact push leaves each word in range either as it was or
        // holding the new value.
        let kept: Vec<bool> = (lo..=hi).map(|p| self.must_ptr(p)).collect();
        self.push(heap)?;
        for (p, old) in (lo..=hi).zip(kept) {
            self.set_must(p, must && (lo == hi || old));
        }
        if let Some(top) = self.ops.last_mut() {
            *top = must;
        }
        Some(())
    }

    /// Pop returning `(may be heap, must be pointer)`.
    fn pop_copy(&mut self) -> Option<(bool, bool)> {
        let tracked = self.ops.last().copied();
        self.pop_n(1)?;
        let heap = (self.lo..=self.hi).any(|p| self.bit(p));
        let must = tracked.unwrap_or(self.lo == self.hi && self.must_ptr(self.lo));
        Some((heap, must))
    }

    fn store_copy(&mut self, slot: usize, heap: bool, must: bool) -> Option<()> {
        self.store(slot, heap)?;
        self.set_must(slot, must);
        Some(())
    }

    /// Slot write (store, dense register, match payload).
    fn set(&mut self, p: usize, heap: bool) -> Option<()> {
        self.write(p, heap, true)
    }

    fn write(&mut self, p: usize, heap: bool, slot: bool) -> Option<()> {
        if p >= MAX_WORDS {
            return None;
        }
        // Operands sit just below the cursor: `[cursor - ops.len(), cursor)`.
        if slot && p + self.ops.len() >= self.lo && p < self.hi {
            self.ops.clear();
        }
        if self.bits.len() <= p {
            self.bits.resize(p + 1, true);
            self.slot.resize(p + 1, true);
        }
        self.bits[p] = heap;
        self.slot[p] = slot;
        self.set_must(p, false);
        Some(())
    }

    fn seek(&mut self, t: usize) {
        self.lo = t;
        self.hi = t;
        self.ops.clear();
    }

    /// The cursor moved without a push / pop (store past it, unpack, a
    /// two-word call result): tracked operands no longer sit at the top.
    fn raise(&mut self, lo: usize, hi: usize) {
        if (lo, hi) != (self.lo, self.hi) {
            self.ops.clear();
        }
        self.lo = lo;
        self.hi = hi;
    }

    /// Operand push at the cursor; an inexact cursor may write any word in
    /// range (which then counts as heap, keeping its slot mark).
    fn push(&mut self, heap: bool) -> Option<()> {
        if self.lo == self.hi {
            self.write(self.lo, heap, false)?;
        } else {
            for p in self.lo..=self.hi {
                let slot = self.slot.get(p).copied().unwrap_or(true);
                self.write(p, true, slot)?;
            }
        }
        self.lo += 1;
        self.hi += 1;
        self.ops.push(false);
        Some(())
    }

    /// Payload push that code later reads as slots (match / `Unpack`).
    fn push_slot(&mut self) -> Option<()> {
        let (lo, hi) = (self.lo, self.hi);
        self.push(true)?;
        for p in lo..=hi {
            self.slot[p] = true;
        }
        Some(())
    }

    fn pop(&mut self) -> Option<bool> {
        self.pop_n(1)?;
        Some((self.lo..=self.hi).any(|p| self.bit(p)))
    }

    fn pop_n(&mut self, n: usize) -> Option<()> {
        self.lo = self.lo.checked_sub(n)?;
        self.hi -= n;
        match self.ops.len().checked_sub(n) {
            Some(k) => self.ops.truncate(k),
            None => self.ops.clear(),
        }
        Some(())
    }

    fn store(&mut self, slot: usize, heap: bool) -> Option<()> {
        self.set(slot, heap)?;
        self.raise(self.lo.max(slot + 1), self.hi.max(slot + 1));
        Some(())
    }

    /// Words a callee (whose frame starts at `base`) may overwrite.
    fn clobber_from(&mut self, base: usize) {
        self.bits.truncate(base);
        self.slot.truncate(base);
        self.ptr.truncate(base);
    }

    /// Slots that may hold heap words: every word below `limit` that may, plus
    /// slot-written words above it (locals past the cursor).
    fn heap_slots(&self, limit: usize, include_above: bool) -> Vec<u16> {
        let top = if include_above {
            limit.max(self.bits.len())
        } else {
            limit
        };
        (0..top)
            .filter(|&p| {
                if p < limit {
                    self.bit(p)
                } else {
                    self.bits[p] && self.slot[p]
                }
            })
            .map(|p| {
                if self.must_ptr(p) {
                    p as u16 | common::PRECISE_SLOT_MUST
                } else {
                    p as u16
                }
            })
            .collect()
    }

    /// Join: a word may be heap if either side says so; the cursor range
    /// covers both.
    fn merge(&mut self, other: &FrameState) -> Option<bool> {
        let (lo, hi) = (self.lo.min(other.lo), self.hi.max(other.hi));
        if hi >= MAX_WORDS {
            return None;
        }
        let len = self.bits.len().min(other.bits.len());
        let mut next: Vec<bool> = (0..len).map(|p| self.bits[p] || other.bits[p]).collect();
        let mut slot: Vec<bool> = (0..len).map(|p| self.slot[p] || other.slot[p]).collect();
        while next.last() == Some(&true) && slot.last() == Some(&true) {
            next.pop();
            slot.pop();
        }
        // Operand bits align at the top of the stack.
        let olen = self.ops.len().min(other.ops.len());
        let ops: Vec<bool> = self.ops[self.ops.len() - olen..]
            .iter()
            .zip(&other.ops[other.ops.len() - olen..])
            .map(|(a, b)| *a && *b)
            .collect();
        let plen = self.ptr.len().min(other.ptr.len());
        let mut ptr: Vec<bool> = (0..plen).map(|p| self.ptr[p] && other.ptr[p]).collect();
        while ptr.last() == Some(&false) {
            ptr.pop();
        }
        let changed = next != self.bits
            || slot != self.slot
            || ptr != self.ptr
            || ops != self.ops
            || (lo, hi) != (self.lo, self.hi);
        self.bits = next;
        self.slot = slot;
        self.ptr = ptr;
        self.ops = ops;
        self.lo = lo;
        self.hi = hi;
        Some(changed)
    }
}

/// Complete heap slots at each recorded PC of the body `[entry, end)`, or
/// `None` when the body cannot be described. Closure bodies compiled inside
/// it (code pointers fed to `MakeFn`) are analysed from their own entry and
/// merged; any other entry into the middle of the body refuses it.
fn analyze_body(
    bytecode: &[Byte],
    constants: &[u64],
    match_arities: &HashMap<u32, u32>,
    entry: u32,
    end: u32,
    seed: &EntrySeed,
    ret_ptr: &HashMap<u32, bool>,
) -> Option<Vec<SlotMap>> {
    let (entry, end) = (entry as usize, end as usize);
    let (arity, coroutine) = entry_arity(bytecode, constants, entry, seed)?;
    let body = Body {
        bytecode,
        constants,
        match_arities,
        entry,
        end,
        params: (!coroutine && seed.params.len() == arity).then_some(&seed.params[..]),
        ret_ptr,
    };
    let mut merged: std::collections::BTreeMap<usize, std::collections::BTreeSet<u16>> =
        std::collections::BTreeMap::new();
    let starts = std::iter::once((entry, arity, coroutine))
        .chain(closure_entries(bytecode, constants, entry, end)?.into_iter().map(|(pc, a)| (pc, a, false)));
    for (start, arity, coroutine) in starts {
        for (pc, slots) in body.analyze_from(start, arity, coroutine)? {
            merged.entry(pc).or_default().extend(slots);
        }
    }
    Some(
        merged
            .into_iter()
            .map(|(pc, slots)| SlotMap {
                pc: pc as u32,
                slots: slots
                    .iter()
                    .copied()
                    .filter(|&s| {
                        !common::precise_slot_must(s)
                            || !slots.contains(&(common::precise_slot_index(s) as u16))
                    })
                    .collect(),
            })
            .collect(),
    )
}

/// Closure bodies inside `(entry, end)`: `CodePtr` targets immediately fed to
/// `MakeFn`, entered with `[captures..., params...]`. `None` when anything
/// else (a plain code pointer, call or coroutine) targets the middle of the
/// body, or closure sites disagree on the frame size.
fn closure_entries(
    bytecode: &[Byte],
    constants: &[u64],
    entry: usize,
    end: usize,
) -> Option<Vec<(usize, usize)>> {
    let mut found: HashMap<usize, usize> = HashMap::new();
    for (from, to) in inbound_targets(bytecode, constants) {
        let (from, to) = (from as usize, to as usize);
        if to <= entry || to >= end {
            continue;
        }
        let b = bytecode.get(from)?;
        if jump_target(b, constants).is_some() {
            continue;
        }
        let make_fn = bytecode.get(from + 1)?;
        if !matches!(*b.bytecode(), Instruction::CodePtr)
            || !matches!(*make_fn.bytecode(), Instruction::MakeFn)
        {
            return None;
        }
        let op = make_fn.operand_u32();
        let words = (op & 0xFF) as usize + ((op >> 16) & 0xFF) as usize + ((op >> 24) & 1) as usize;
        if *found.entry(to).or_insert(words) != words {
            return None;
        }
    }
    let mut out: Vec<(usize, usize)> = found.into_iter().collect();
    out.sort_unstable();
    Some(out)
}

/// One body's bytecode range and the side tables the dataflow reads.
struct Body<'a> {
    bytecode: &'a [Byte],
    constants: &'a [u64],
    match_arities: &'a HashMap<u32, u32>,
    entry: usize,
    end: usize,
    /// Must-pointer entry slots (signature kinds matching the entry arity).
    params: Option<&'a [bool]>,
    /// Callee entry PC → its one-word result is a must-pointer.
    ret_ptr: &'a HashMap<u32, bool>,
}

impl Body<'_> {
    /// Fixpoint from `start` (with `arity` live words) to the recorded slots.
    fn analyze_from(
        &self,
        start: usize,
        arity: usize,
        coroutine: bool,
    ) -> Option<Vec<(usize, Vec<u16>)>> {
        let (entry, end) = (self.entry, self.end);
        let mut states: HashMap<usize, FrameState> = HashMap::new();
        let mut work = vec![start];
        // Parameters are live words; the signature may prove some pointers.
        let ptr = match self.params {
            Some(p) if start == entry => p.to_vec(),
            _ => Vec::new(),
        };
        states.insert(
            start,
            FrameState {
                lo: arity,
                hi: arity,
                bits: vec![true; arity],
                slot: vec![true; arity],
                ptr,
                ops: Vec::new(),
            },
        );
        let step_at = |pc: usize, st: &mut FrameState| transfer(self, pc, coroutine, st);
        let mut recorded: HashSet<usize> = HashSet::new();
        let mut steps = 0usize;
        while let Some(pc) = work.pop() {
            steps += 1;
            if steps > 200_000 {
                return None;
            }
            let mut st = states.get(&pc)?.clone();
            let step = step_at(pc, &mut st)?;
            if step.record.is_some() {
                recorded.insert(pc);
            }
            let jump_state = step.jump_state.clone().unwrap_or_else(|| st.clone());
            let edges = step
                .fallthrough
                .then(|| (pc + step.width, st.clone()))
                .into_iter()
                .chain(step.jump.map(|t| (t, jump_state)));
            for (succ, out) in edges {
                if succ < entry || succ >= end {
                    return None;
                }
                match states.get_mut(&succ) {
                    Some(existing) => {
                        if existing.merge(&out)? {
                            work.push(succ);
                        }
                    }
                    None => {
                        states.insert(succ, out);
                        work.push(succ);
                    }
                }
            }
        }
        // Recorded sets must reflect the fixpoint: recompute from final states.
        let mut out = Vec::with_capacity(recorded.len());
        for pc in recorded {
            let mut st = states.get(&pc)?.clone();
            out.push((pc, step_at(pc, &mut st)?.record?));
        }
        Some(out)
    }
}

/// How a body's entry state is known besides its callers' `CALL`s.
struct EntrySeed {
    /// The prologue `JMP` target (`main` or setup).
    prologue_entry: usize,
    /// Codegen's entry height (params, `self`, dictionaries) for this body.
    declared: Option<usize>,
    /// Must-pointer parameters from the signature (used only when their
    /// count matches the entry arity).
    params: Vec<bool>,
}

/// Words on the frame when the body at `entry` starts, and whether it runs
/// as a coroutine: its callers' `CALL` / `TailCall` arity, the `MakeCoro`
/// arity (plus the send word the first resume pushes before a store), `0` for
/// `main` entered from the prologue, or the declared entry height when no
/// bytecode references the body (only the host calls it: tests, finalizers,
/// callbacks). Bodies entered any other way (jump, closure code pointer) or
/// with differing arities are refused.
fn entry_arity(
    bytecode: &[Byte],
    constants: &[u64],
    entry: usize,
    seed: &EntrySeed,
) -> Option<(usize, bool)> {
    if entry == seed.prologue_entry && entered_by_prologue_only(bytecode, constants, entry) {
        return Some((0, false));
    }
    let mut referenced = false;
    let mut code_ref = false;
    // Frame words of closures built on this entry (`[captures..., params]`).
    let mut closure_words = 0usize;
    let mut coro_arity = None;
    let mut arity = None;
    for (from, b) in bytecode.iter().enumerate() {
        let inst = *b.bytecode();
        let target = match inst {
            Instruction::CALL if b.call_parts().1 == 0 => None,
            Instruction::CALL | Instruction::TailCall | Instruction::MakeCoro => {
                Some(b.call_parts().1)
            }
            Instruction::CodePtr | Instruction::MakePolyFn | Instruction::MakePolyFnCapture => {
                Some(b.operand_u32() as usize)
            }
            _ => jump_target(b, constants),
        };
        if target != Some(entry) {
            continue;
        }
        referenced = true;
        // Code pointers and poly-fn values are called through `CallIndirect`
        // with the declared params and dictionaries; a pointer fed to `MakeFn`
        // also prepends its captures.
        if matches!(
            inst,
            Instruction::CodePtr | Instruction::MakePolyFn | Instruction::MakePolyFnCapture
        ) {
            code_ref = true;
            if let Some(make_fn) = bytecode.get(from + 1)
                && matches!(*make_fn.bytecode(), Instruction::MakeFn)
            {
                let op = make_fn.operand_u32();
                let words =
                    (op & 0xFF) as usize + ((op >> 16) & 0xFF) as usize + ((op >> 24) & 1) as usize;
                closure_words = closure_words.max(words);
            }
            continue;
        }
        let slot = match inst {
            Instruction::CALL | Instruction::TailCall => &mut arity,
            Instruction::MakeCoro => &mut coro_arity,
            _ => return None,
        };
        let a = b.call_parts().0;
        match *slot {
            None => *slot = Some(a),
            Some(prev) if prev == a => {}
            Some(_) => return None,
        }
    }
    match (arity, coro_arity) {
        (Some(_), Some(_)) => None,
        (None, Some(a)) if !code_ref => {
            Some((a + usize::from(receives_send(bytecode, entry)), true))
        }
        (None, Some(_)) => None,
        // Overestimating the entry height only roots a few stale words.
        (Some(a), None) if code_ref => Some((a.max(seed.declared?).max(closure_words), false)),
        (Some(a), None) => Some((a, false)),
        (None, None) if referenced && !code_ref => None,
        (None, None) => seed.declared.map(|a| (a.max(closure_words), false)),
    }
}

/// A resume pushes the sent value when the op it resumes at stores it.
fn receives_send(bytecode: &[Byte], pc: usize) -> bool {
    bytecode
        .get(pc)
        .is_some_and(|b| matches!(*b.bytecode(), Instruction::STORE | Instruction::StorePop))
}

/// `main` shape: `CALL 0 target=0` at PC 0 opens a fresh frame at base 0,
/// the prologue `JMP` at PC 1 (patched to the prologue target after bind)
/// is the only transfer into `entry`.
fn entered_by_prologue_only(bytecode: &[Byte], constants: &[u64], entry: usize) -> bool {
    let opens_frame = bytecode.first().is_some_and(|b| {
        matches!(*b.bytecode(), Instruction::CALL) && b.call_parts() == (0, 0)
    });
    let prologue_jmp = bytecode
        .get(1)
        .is_some_and(|b| matches!(*b.bytecode(), Instruction::JMP));
    opens_frame
        && prologue_jmp
        && inbound_targets(bytecode, constants)
            .iter()
            .all(|&(from, to)| to as usize != entry || from == 1)
}

struct Step {
    /// Words this op occupies (two-word dense packs).
    width: usize,
    fallthrough: bool,
    jump: Option<usize>,
    /// State on the jump edge when it differs from the fall-through state.
    jump_state: Option<FrameState>,
    record: Option<Vec<u16>>,
}

/// Apply one op to `st`. `None` refuses the body (unmodeled op).
fn transfer(body: &Body, pc: usize, coroutine: bool, st: &mut FrameState) -> Option<Step> {
    let b = body.bytecode.get(pc)?;
    let tail_word = body.bytecode.get(pc + 1);
    let next_receives = receives_send(body.bytecode, pc + 1);
    let (constants, match_arities, end) = (body.constants, body.match_arities, body.end);
    use Instruction::*;
    let mut step = Step {
        width: 1,
        fallthrough: true,
        jump: None,
        jump_state: None,
        record: None,
    };
    let inst = *b.bytecode();
    match inst {
        NOOP | DATA => {}
        CastIntToFloat | CastFloatToInt | CastIntToByte | CastByteToInt | CastIntToBool
        | CastBoolToInt | NOT | LogNot | NEG | NEGF | ArrayLen => {
            st.pop()?;
            st.push(false)?;
        }
        // Literal `0` is a valid must-pointer value (null / `None`): joined
        // with an allocation the word stays a must-pointer.
        CONST if b.operand_u32() == 0 => st.push_copy(false, true)?,
        CONST | CodePtr => st.push(false)?,
        LOAD => {
            for i in 0..b.load_store_count() {
                let slot = b.load_store_slot_at(i) as usize;
                let (heap, must) = (st.bit(slot), st.must_ptr(slot));
                st.push_copy(heap, must)?;
            }
        }
        STORE | StorePop => {
            for i in 0..b.load_store_count() {
                let slot = b.load_store_slot_at(i) as usize;
                let (heap, must) = st.pop_copy()?;
                st.store_copy(slot, heap, must)?;
            }
        }
        Seek => st.seek(b.operand_u32() as usize),
        DUPLICATE => {
            let (heap, must) = st.pop_copy()?;
            st.push_copy(heap, must)?;
            st.push_copy(heap, must)?;
        }
        POP | PRINT | StoreStatic => {
            st.pop()?;
        }
        LoadStatic => st.push(true)?,
        ADD | SUB | MUL | DIV | MOD | LE | LEQ | GT | GEQ | EQ | NEQ | Pow | BITAND | BITOR
        | ADDF | SUBF | MULF | DIVF | MODF | LEF | LEQF | GTF | GEQF | PowF | SHL | SHR | XOR
        | AND | OR => {
            st.pop_n(2)?;
            st.push(false)?;
        }
        BinSlotImm | BinSlotSlot => st.push(false)?,
        BinSlotImmStore => {
            let (_, _, pool_idx) = b.bin_slot_imm_store_parts();
            let dest = (constants.get(pool_idx)? >> 32) as usize;
            st.store(dest, false)?;
        }
        BinSlotSlotStore => {
            let (_, _, _, dest) = b.bin_slot_slot_store_parts();
            st.store(dest, false)?;
        }
        JMP => {
            step.fallthrough = false;
            step.jump = Some(jump_target(b, constants)?);
        }
        JMPF | JMPT | LogNotJmpf | LogNotJmpt => {
            st.pop()?;
            step.jump = Some(jump_target(b, constants)?);
        }
        CmpJmpf | CmpJmpt => {
            st.pop_n(2)?;
            step.jump = Some(jump_target(b, constants)?);
        }
        BinSlotImmJmpf | BinSlotImmJmpt | BinSlotSlotJmpf | BinSlotSlotJmpt => {
            step.jump = Some(jump_target(b, constants)?);
        }
        RETURN | LoadReturnSlot | ConstReturnImm | BinReturn | ReturnPair | TailCall | HALT
        | Panic => {
            step.fallthrough = false;
        }
        // Allocates (a safepoint with the payload still on the stack), then
        // returns the new enum.
        MakeEnumReturn | MakeEnumReturnK => {
            st.pop_n(b.make_arity() as usize)?;
            st.push_ptr()?;
            step.record = Some(st.heap_slots(st.hi, true));
            step.fallthrough = false;
        }
        CALL => {
            let (arity, target) = b.call_parts();
            if target == 0 || pc + 1 >= end {
                return None;
            }
            st.pop_n(arity)?;
            step.record = Some(st.heap_slots(st.hi, false));
            st.clobber_from(st.lo);
            let ret_words = b.call_ret_words();
            let must = ret_words == 1 && body.ret_ptr.get(&(target as u32)) == Some(&true);
            for _ in 0..ret_words {
                st.push_copy(true, must)?;
            }
        }
        // Pops the target, dictionaries and args; the callee frame starts at
        // their base. A partial application instead pushes a new function
        // there and hits a safepoint, so the record covers the base word too.
        CallIndirect => {
            let packed = b.operand_u32();
            let words = 1 + (packed & 0xFFFF) as usize + ((packed >> 16) & 0xFFFF) as usize;
            st.pop_n(words)?;
            step.record = Some(st.heap_slots(st.hi + 1, true));
            st.clobber_from(st.lo);
            st.push(true)?;
            // The callee's return width is not encoded: one or two words.
            st.write(st.hi, true, false)?;
            st.raise(st.lo, st.hi + 1);
        }
        // Suspends: the frame's words below the cursor are saved and restored
        // on resume; anything above is gone. Outside a coroutine the value
        // would stay on the stack, so only `MakeCoro` bodies model it.
        YieldCoro => {
            if !coroutine || st.lo != st.hi {
                return None;
            }
            st.pop()?;
            st.clobber_from(st.lo);
            if next_receives {
                st.push(true)?;
            }
        }
        // Runs another coroutine above the cursor; its yield / return value
        // lands at the base.
        ResumeCoro => {
            st.pop_n(1 + (b.operand_u32() & 1) as usize)?;
            st.clobber_from(st.lo);
            st.push(true)?;
        }
        DoneCoro => {
            st.pop()?;
            st.push(false)?;
        }
        INC | DEC => {
            st.set(b.inc_dec_parts().0, false)?;
            st.push(false)?;
        }
        // In-place payload unpack when the enum matches (cursor floor too).
        UnpackAt => {
            let op = b.operand_u32();
            let (slot, arity) = ((op & 0xFFFF) as usize, (op >> 16) as usize);
            for p in slot..slot + arity {
                st.set(p, true)?;
            }
            st.raise(st.lo, st.hi.max(slot + arity));
        }
        DictEntries => {
            st.pop()?;
            st.push(true)?;
            step.record = Some(st.heap_slots(st.hi, true));
        }
        MakePolyFn | MakePolyFnCapture => {
            if matches!(inst, MakePolyFnCapture) {
                st.pop_n((b.operand_u32() & 0xFF) as usize + 1)?;
            }
            st.push_ptr()?;
            step.record = Some(st.heap_slots(st.hi, true));
        }
        // Pops `[captures..., filled..., mask, entry]`, pushes the closure.
        MakeFn => {
            let op = b.operand_u32();
            st.pop_n((op & 0xFF) as usize + ((op >> 8) & 0xFF) as usize + 2)?;
            st.push_ptr()?;
            step.record = Some(st.heap_slots(st.hi, true));
        }
        MakeCoro => {
            st.pop_n(b.call_parts().0)?;
            st.push_ptr()?;
            step.record = Some(st.heap_slots(st.hi, true));
        }
        HostInvoke => {
            let arity = (b.operand_u32() & 0xFFFF) as usize;
            st.pop_n(arity + 1)?;
            st.push(true)?;
            step.record = Some(st.heap_slots(st.hi, true));
        }
        MakeTuple | MakeArray | MakeEnum => {
            st.pop_n((b.operand_u32() & 0xFFFF) as usize)?;
            st.push_ptr()?;
            step.record = Some(st.heap_slots(st.hi, true));
        }
        MakeTupleK | MakeEnumK => {
            st.pop_n(b.make_arity() as usize)?;
            st.push_ptr()?;
            step.record = Some(st.heap_slots(st.hi, true));
        }
        InitTyped | INIT | STRING => {
            st.push_ptr()?;
            step.record = Some(st.heap_slots(st.hi, true));
        }
        BoxValue | STRINGIFY => {
            st.pop()?;
            st.push_ptr()?;
            step.record = Some(st.heap_slots(st.hi, true));
        }
        FORMAT => {
            let n = b.operand_u32() as usize;
            if n != 0 {
                st.pop_n(n + 1)?;
                st.push_ptr()?;
            }
            step.record = Some(st.heap_slots(st.hi, true));
        }
        MakeDict => {
            st.pop_n(2 * (b.operand_u32() & 0xFFFF) as usize)?;
            st.push_ptr()?;
            step.record = Some(st.heap_slots(st.hi, true));
        }
        ArrayPush => {
            st.pop_n(2)?;
            st.push_ptr()?;
            step.record = Some(st.heap_slots(st.hi, true));
        }
        UnboxValue | LoadField => {
            st.pop()?;
            st.push(true)?;
        }
        // Stamps metadata on the enum / array at TOS; no allocation, no
        // safepoint.
        TagEnumType | TagArrayKind => {
            let (heap, must) = st.pop_copy()?;
            st.push_copy(heap, must)?;
        }
        Index | IndexUnchecked | GetField => {
            st.pop_n(2)?;
            st.push(true)?;
        }
        StoreIndex | StoreIndexUnchecked => {
            st.pop_n(3)?;
            st.push(true)?;
        }
        // Match: scrutinee stays on a miss; a hit pops it and pushes the payload.
        JumpIfMatch => {
            let arity = *match_arities.get(&(pc as u32))? as usize;
            let target = *constants.get((b.operand_u32() & 0xFFFF) as usize)? as usize;
            let mut hit = st.clone();
            hit.pop()?;
            for _ in 0..arity {
                hit.push_slot()?;
            }
            step.jump = Some(target);
            step.jump_state = Some(hit);
        }
        Unpack => {
            st.pop()?;
            for _ in 0..b.operand_u32() {
                st.push_slot()?;
            }
        }
        // Dense register ops write frame slots without moving the cursor.
        DenseBin | DenseCmp => st.set(b.dense_abc_parts().1, false)?,
        DenseUnary | DenseCast => st.set(b.dense_unary_parts().1, false)?,
        DenseConst => {
            let (_, dest, value, pooled) = b.dense_const_parts();
            st.set_copy(dest, false, value == 0 && !pooled)?;
        }
        DenseArrayLen => st.set(b.dense_move_parts().0, false)?,
        DenseMove => {
            let (dest, src) = b.dense_move_parts();
            let (heap, must) = (st.bit(src), st.must_ptr(src));
            st.set_copy(dest, heap, must)?;
        }
        DenseIndex | DenseFieldLoad => st.set(b.dense_abc_parts().1, true)?,
        DenseStoreIndex | DenseFieldStore => {}
        // SIMD lanes live in VM vector registers; only a reduce writes a slot.
        VLoad | VStore | VBin | VMove | VFma => {}
        VReduce => st.set(b.dense_abc_parts().1, false)?,
        DensePush => {
            let (arity, base) = b.dense_move_parts();
            for i in 0..arity {
                let (heap, must) = (st.bit(base + i), st.must_ptr(base + i));
                st.push_copy(heap, must)?;
            }
        }
        // `DenseMakeK` keeps `dest` in the same byte as `DenseMake`.
        DenseMake | DenseMakeK | DenseArrayPush => {
            st.set_ptr(b.dense_abc_parts().1)?;
            step.record = Some(st.heap_slots(st.hi, true));
        }
        DenseMakeObject => {
            let (dest, _, _) = common::dense::unpack_make_object(b.operand_u32());
            st.set_ptr(dest as usize)?;
            step.record = Some(st.heap_slots(st.hi, true));
        }
        DenseBin2 | DenseBinJmpf | DenseIndexJmpf => {
            if pc + 2 > end {
                return None;
            }
            match inst {
                DenseIndexJmpf => st.set(b.dense_abc_parts().1, true)?,
                _ => st.set(b.dense_abc_parts().1, false)?,
            }
            step.width = 2;
            let tail = tail_word?;
            if matches!(inst, DenseBin2) {
                if !matches!(*tail.bytecode(), DenseBin) {
                    return None;
                }
                st.set(tail.dense_abc_parts().1, false)?;
            } else {
                if !matches!(
                    *tail.bytecode(),
                    BinSlotImmJmpf | BinSlotImmJmpt | BinSlotSlotJmpf | BinSlotSlotJmpt
                ) {
                    return None;
                }
                step.jump = Some(jump_target(tail, constants)?);
            }
        }
        SetField => {
            let n = if common::set_field_slot_index(b.operand_u32()).is_some() { 2 } else { 3 };
            // Pushes back the stored value (the lowest popped word).
            let tracked = st.ops.len().checked_sub(n).map(|i| st.ops[i]);
            st.pop_n(n)?;
            let exact = st.lo == st.hi;
            let heap = !exact || st.bit(st.lo);
            let must = tracked.unwrap_or(exact && st.must_ptr(st.lo));
            st.push_copy(heap, must)?;
        }
        _ => return None,
    }
    Some(step)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(i: Instruction) -> Byte {
        Byte::new(i)
    }
    fn load(slot: u32) -> Byte {
        op(Instruction::LOAD).with_load_store_slot(slot)
    }
    fn store(slot: u32) -> Byte {
        op(Instruction::STORE).with_load_store_slot(slot)
    }
    fn konst(v: u32) -> Byte {
        op(Instruction::CONST).with_operand_u32(v)
    }
    fn call(arity: u32, target: u32) -> Byte {
        op(Instruction::CALL).with_call_packed(arity, target)
    }

    /// `[0] CALL 1 → 2; [1] HALT; [2..] body`, bound as `f` at PC 2.
    fn bind(body: &[Byte]) -> Vec<PreciseFrameMap> {
        bind_with(body, FnWordKinds::default())
    }

    /// [`bind`] with `f`'s signature word kinds.
    fn bind_with(body: &[Byte], kinds: FnWordKinds) -> Vec<PreciseFrameMap> {
        let mut code = vec![call(1, 2), op(Instruction::HALT)];
        code.extend_from_slice(body);
        let entries = vec![("f".to_string(), 2)];
        let fn_kinds = HashMap::from([("f".to_string(), kinds)]);
        bind_precise_frames(
            &HashSet::new(),
            &HashSet::new(),
            &code,
            &[],
            &HashMap::new(),
            &entries,
            &HashMap::new(),
            u32::MAX,
            &fn_kinds,
        )
    }

    /// Slot indices (flags stripped), ascending.
    fn slots_at(maps: &[PreciseFrameMap], pc: u32) -> Option<Vec<u16>> {
        precise_map_for_pc(maps, pc)?.slots_at_pc(pc).map(|s| {
            let mut v: Vec<u16> = s.iter().map(|&w| common::precise_slot_index(w) as u16).collect();
            v.sort_unstable();
            v
        })
    }

    /// Slots flagged as definitely holding a pointer, ascending.
    fn must_at(maps: &[PreciseFrameMap], pc: u32) -> Option<Vec<u16>> {
        precise_map_for_pc(maps, pc)?.slots_at_pc(pc).map(|s| {
            let mut v: Vec<u16> = s
                .iter()
                .filter(|&&w| common::precise_slot_must(w))
                .map(|&w| common::precise_slot_index(w) as u16)
                .collect();
            v.sort_unstable();
            v
        })
    }

    use common::precise_map_for_pc;

    /// Rows may still carry a frame extent; none describes heap slots.
    fn no_precise(maps: &[PreciseFrameMap]) -> bool {
        maps.iter().all(|m| m.any_pc.is_none() && m.at_pc.is_empty())
    }

    #[test]
    fn alloc_safepoint_keeps_heap_words_and_drops_numbers() {
        // slot 0 = param (unknown), slot 1 = int, slot 2 = fresh array.
        let maps = bind(&[
            konst(7),
            store(1),
            load(0),
            op(Instruction::MakeArray).with_operand_u32(1),
            store(2),
            load(1),
            op(Instruction::RETURN),
        ]);
        // State after MakeArray at PC 5: [param, int, array].
        assert_eq!(slots_at(&maps, 5), Some(vec![0, 2]));
    }

    #[test]
    fn call_records_caller_words_below_args() {
        let maps = bind(&[
            konst(1),
            store(1),
            op(Instruction::Seek).with_operand_u32(2),
            konst(3),
            call(1, 2),
            op(Instruction::RETURN),
        ]);
        // Caller words below the callee base (2): param 0 heap, slot 1 int.
        assert_eq!(slots_at(&maps, 6), Some(vec![0]));
    }

    #[test]
    fn allocation_copies_stay_must_pointers() {
        // [2] InitTyped [3] store 0 [4] load 0 [5] store 1 [6] InitTyped [7] RETURN
        let maps = bind(&[
            op(Instruction::InitTyped),
            store(0),
            load(0),
            store(1),
            op(Instruction::InitTyped),
            op(Instruction::RETURN),
        ]);
        assert_eq!(must_at(&maps, 6), Some(vec![0, 1, 2]));
    }

    #[test]
    fn join_with_a_constant_is_not_a_must_pointer() {
        // [2] CONST [3] JMPF 7 [4] InitTyped [5] store 0 [6] JMP 9
        // [7] CONST [8] store 0 [9] InitTyped [10] RETURN
        let maps = bind(&[
            konst(1),
            op(Instruction::JMPF).with_operand_u32(7),
            op(Instruction::InitTyped),
            store(0),
            op(Instruction::JMP).with_operand_u32(9),
            konst(5),
            store(0),
            op(Instruction::InitTyped),
            op(Instruction::RETURN),
        ]);
        let may = slots_at(&maps, 9).expect("safepoint row");
        assert!(may.contains(&0), "the allocation path may leave a heap word: {may:?}");
        let must = must_at(&maps, 9).expect("safepoint row");
        assert!(!must.contains(&0), "one path stores an int: {must:?}");
    }

    #[test]
    fn join_of_differing_heights_covers_the_range() {
        // then-arm leaves one extra word; both arms reach an alloc.
        let maps = bind(&[
            load(0),
            op(Instruction::JMPF).with_operand_u32(6),
            load(0),
            load(0),
            op(Instruction::JMP).with_operand_u32(7),
            op(Instruction::NOOP),
            konst(0),
            op(Instruction::MakeArray).with_operand_u32(1),
            op(Instruction::RETURN),
        ]);
        let at = slots_at(&maps, 9).expect("alloc recorded");
        assert!(at.contains(&0) && at.contains(&1) && at.contains(&2), "{at:?}");
    }

    #[test]
    fn unmodeled_op_refuses_the_body() {
        let maps = bind(&[
            load(0),
            op(Instruction::MakeArray).with_operand_u32(1),
            op(Instruction::FfiInvoke),
            op(Instruction::RETURN),
        ]);
        assert!(no_precise(&maps));
    }

    #[test]
    fn host_called_body_uses_declared_entry_height() {
        // `[0] HALT; [1..] body` with no bytecode reference to PC 1.
        let mut code = vec![op(Instruction::HALT)];
        code.extend_from_slice(&[
            load(0),
            op(Instruction::MakeArray).with_operand_u32(1),
            op(Instruction::RETURN),
        ]);
        let entries = vec![("t".to_string(), 1)];
        let declared = HashMap::from([("t".to_string(), 1)]);
        let maps = bind_precise_frames(
            &HashSet::new(),
            &HashSet::new(),
            &code,
            &[],
            &HashMap::new(),
            &entries,
            &declared,
            u32::MAX,
            &HashMap::new(),
        );
        assert_eq!(slots_at(&maps, 2), Some(vec![0, 1]));

        // Without a declared height, a code-pointer-only body stays unmapped.
        code[0] = op(Instruction::CodePtr).with_operand_u32(1);
        let maps = bind_precise_frames(
            &HashSet::new(),
            &HashSet::new(),
            &code,
            &[],
            &HashMap::new(),
            &entries,
            &HashMap::new(),
            u32::MAX,
            &HashMap::new(),
        );
        assert!(no_precise(&maps));
    }

    #[test]
    fn coroutine_body_keeps_words_across_a_yield() {
        // `[0] MakeCoro 0 → 2; [1] HALT; [2..] body`.
        let mut code = vec![
            op(Instruction::MakeCoro).with_call_packed(0, 2),
            op(Instruction::HALT),
        ];
        code.extend_from_slice(&[
            op(Instruction::InitTyped),
            store(0),
            konst(1),
            op(Instruction::YieldCoro),
            store(1),
            op(Instruction::InitTyped),
            op(Instruction::RETURN),
        ]);
        let entries = vec![("co".to_string(), 2)];
        let maps = bind_precise_frames(
            &HashSet::new(),
            &HashSet::new(),
            &code,
            &[],
            &HashMap::new(),
            &entries,
            &HashMap::new(),
            u32::MAX,
            &HashMap::new(),
        );
        // After resume: saved local 0, the sent value in slot 1, a new object.
        assert_eq!(slots_at(&maps, 7), Some(vec![0, 1, 2]));
        // Allocations are definitely pointers; the sent value is not known.
        assert_eq!(must_at(&maps, 7), Some(vec![0, 2]));
    }

    #[test]
    fn closure_body_is_analysed_from_its_own_entry() {
        // Body at 2 builds a one-capture closure whose code sits at 10.
        let maps = bind(&[
            load(0),
            konst(0),
            op(Instruction::CodePtr).with_operand_u32(10),
            op(Instruction::MakeFn).with_operand_u32(1 | (1 << 16)),
            store(1),
            konst(0),
            op(Instruction::RETURN),
            op(Instruction::HALT),
            // closure frame: [capture, param]
            load(0),
            op(Instruction::MakeArray).with_operand_u32(1),
            op(Instruction::RETURN),
        ]);
        // Enclosing MakeFn at 5: param 0 and the new closure at 1.
        assert_eq!(slots_at(&maps, 5), Some(vec![0, 1]));
        // Closure MakeArray at 11: capture, param, array.
        assert_eq!(slots_at(&maps, 11), Some(vec![0, 1, 2]));
    }

    #[test]
    fn plain_code_pointer_into_the_body_refuses_it() {
        let maps = bind(&[
            op(Instruction::CodePtr).with_operand_u32(5),
            op(Instruction::MakeArray).with_operand_u32(1),
            op(Instruction::RETURN),
            load(0),
            op(Instruction::RETURN),
        ]);
        assert!(no_precise(&maps));
    }

    #[test]
    fn only_marked_bodies_carry_a_frame_extent() {
        let code = vec![
            call(1, 2),
            op(Instruction::HALT),
            load(0),
            store(6),
            op(Instruction::MakeArray).with_operand_u32(1),
            op(Instruction::RETURN),
        ];
        let entries = vec![("f".to_string(), 2)];
        let bind_with = |needs: HashSet<String>| {
            bind_precise_frames(
                &HashSet::new(),
                &needs,
                &code,
                &[],
                &HashMap::new(),
                &entries,
                &HashMap::new(),
                u32::MAX,
                &HashMap::new(),
            )
        };
        let plain = bind_with(HashSet::new());
        assert!(plain.iter().all(|m| m.frame_words == 0));
        let marked = bind_with(HashSet::from(["f".to_string()]));
        assert_eq!(marked[0].frame_words, 7);
    }

    #[test]
    fn frame_extent_is_the_highest_slot_touched() {
        let body = [
            load(0),
            store(7),
            op(Instruction::DenseMove).with_operand_u32((9 << 8) | 1),
            op(Instruction::RETURN),
        ];
        let dest = body[2].dense_move_parts().0;
        assert_eq!(frame_extent(&body, &[]), Some(dest.max(7) as u32 + 1));
        let unknown = [load(0), op(Instruction::FfiInvoke), op(Instruction::RETURN)];
        assert_eq!(frame_extent(&unknown, &[]), None);
    }

    #[test]
    fn entry_mid_body_refuses_the_body() {
        let mut code = vec![call(1, 2), call(1, 4)];
        code.extend_from_slice(&[
            load(0),
            op(Instruction::MakeArray).with_operand_u32(1),
            op(Instruction::RETURN),
        ]);
        let entries = vec![("f".to_string(), 2)];
        let maps =
            bind_precise_frames(&HashSet::new(), &HashSet::new(), &code, &[], &HashMap::new(), &entries, &HashMap::new(), u32::MAX, &HashMap::new());
        assert!(no_precise(&maps));
    }
    #[test]
    fn pointer_params_from_the_signature_are_must_pointers() {
        let body = [op(Instruction::InitTyped), op(Instruction::RETURN)];
        let ptr = FnWordKinds {
            params: vec![common::WORD_POINTER],
            ret: common::WORD_UNKNOWN,
        };
        assert_eq!(must_at(&bind_with(&body, ptr), 2), Some(vec![0, 1]));
        // Without a signature the param is only a may-pointer.
        assert_eq!(must_at(&bind(&body), 2), Some(vec![1]));
    }

    #[test]
    fn pointer_returning_call_result_is_a_must_pointer() {
        // [2] load 0 [3] CALL f [4] store 1 [5] InitTyped [6] RETURN
        let body = [
            load(0),
            call(1, 2),
            store(1),
            op(Instruction::InitTyped),
            op(Instruction::RETURN),
        ];
        let ptr = FnWordKinds {
            params: vec![common::WORD_SCALAR],
            ret: common::WORD_POINTER,
        };
        assert_eq!(must_at(&bind_with(&body, ptr), 5), Some(vec![1, 2]));
        assert_eq!(must_at(&bind(&body), 5), Some(vec![2]));
    }

    #[test]
    fn null_joined_with_an_allocation_stays_a_must_pointer() {
        // [2] CONST [3] JMPF 7 [4] InitTyped [5] store 0 [6] JMP 9
        // [7] CONST 0 [8] store 0 [9] InitTyped [10] RETURN
        let maps = bind(&[
            konst(1),
            op(Instruction::JMPF).with_operand_u32(7),
            op(Instruction::InitTyped),
            store(0),
            op(Instruction::JMP).with_operand_u32(9),
            konst(0),
            store(0),
            op(Instruction::InitTyped),
            op(Instruction::RETURN),
        ]);
        assert!(must_at(&maps, 9).expect("safepoint row").contains(&0));
    }

    #[test]
    fn push_pop_pair_keeps_its_kind_under_an_inexact_cursor() {
        // The then-arm leaves one extra word, so at [6] the cursor is 1 or 2.
        // [6] InitTyped [7] store 5 [8] InitTyped [9] RETURN
        let maps = bind(&[
            load(0),
            op(Instruction::JMPF).with_operand_u32(6),
            load(0),
            op(Instruction::JMP).with_operand_u32(6),
            op(Instruction::InitTyped),
            store(5),
            op(Instruction::InitTyped),
            op(Instruction::RETURN),
        ]);
        assert!(must_at(&maps, 8).expect("safepoint row").contains(&5));
    }

    #[test]
    fn set_field_result_keeps_the_stored_value_kind() {
        // [2] InitTyped (value) [3] InitTyped (target) [4] SetField
        // [5] store 3 [6] InitTyped [7] RETURN
        let maps = bind(&[
            op(Instruction::InitTyped),
            op(Instruction::InitTyped),
            op(Instruction::SetField).with_operand_u32(common::pack_set_field_slot(0)),
            store(3),
            op(Instruction::InitTyped),
            op(Instruction::RETURN),
        ]);
        assert!(must_at(&maps, 6).expect("safepoint row").contains(&3));
    }

    #[test]
    fn make_enum_return_records_its_allocation() {
        // [2] InitTyped [3] load 0 [4] MakeEnumReturnK arity 2
        let maps = bind(&[
            op(Instruction::InitTyped),
            load(0),
            op(Instruction::MakeEnumReturnK).with_operand_u32(2),
        ]);
        assert_eq!(slots_at(&maps, 4), Some(vec![0, 1]));
        assert_eq!(must_at(&maps, 4), Some(vec![1]));
    }

}
