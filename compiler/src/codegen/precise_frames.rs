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

/// Precise maps for every body in `entries` that can be described.
pub fn bind_precise_frames(
    heap_free: &HashSet<String>,
    bytecode: &[Byte],
    constants: &[u64],
    match_arities: &HashMap<u32, u32>,
    entries: &[(String, u32)],
    prologue_entry: u32,
) -> Vec<PreciseFrameMap> {
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
        let named_heap_free = entries
            .iter()
            .any(|(n, pc)| *pc == entry && heap_free.contains(n));
        if named_heap_free && ends_in_exit(body) && body.iter().all(|b| !puts_heap_word(*b.bytecode())) {
            out.push(PreciseFrameMap {
                entry_pc: entry,
                end_pc: end,
                any_pc: Some(Vec::new()),
                at_pc: Vec::new(),
            });
            continue;
        }
        let enters_mid_body = foreign
            .iter()
            .any(|&(from, to)| to > entry && to < end && !(from >= entry && from < end));
        if enters_mid_body {
            continue;
        }
        if let Some(at_pc) = analyze_body(bytecode, constants, match_arities, entry, end, prologue_entry)
            && !at_pc.is_empty()
        {
            out.push(PreciseFrameMap {
                entry_pc: entry,
                end_pc: end,
                any_pc: None,
                at_pc,
            });
        }
    }
    out
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
}

/// Words the analysis refuses to track (keeps `u16` slots in range).
const MAX_WORDS: usize = 4096;

impl FrameState {
    fn bit(&self, p: usize) -> bool {
        self.bits.get(p).copied().unwrap_or(true)
    }

    /// Slot write (store, dense register, match payload).
    fn set(&mut self, p: usize, heap: bool) -> Option<()> {
        self.write(p, heap, true)
    }

    fn write(&mut self, p: usize, heap: bool, slot: bool) -> Option<()> {
        if p >= MAX_WORDS {
            return None;
        }
        if self.bits.len() <= p {
            self.bits.resize(p + 1, true);
            self.slot.resize(p + 1, true);
        }
        self.bits[p] = heap;
        self.slot[p] = slot;
        Some(())
    }

    fn seek(&mut self, t: usize) {
        self.lo = t;
        self.hi = t;
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
        Some(())
    }

    fn store(&mut self, slot: usize, heap: bool) -> Option<()> {
        self.set(slot, heap)?;
        self.lo = self.lo.max(slot + 1);
        self.hi = self.hi.max(slot + 1);
        Some(())
    }

    /// Words a callee (whose frame starts at `base`) may overwrite.
    fn clobber_from(&mut self, base: usize) {
        self.bits.truncate(base);
        self.slot.truncate(base);
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
            .map(|p| p as u16)
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
        let changed =
            next != self.bits || slot != self.slot || (lo, hi) != (self.lo, self.hi);
        self.bits = next;
        self.slot = slot;
        self.lo = lo;
        self.hi = hi;
        Some(changed)
    }
}

/// Complete heap slots at each recorded PC of the body `[entry, end)`, or
/// `None` when the body uses an op the dataflow does not model.
fn analyze_body(
    bytecode: &[Byte],
    constants: &[u64],
    match_arities: &HashMap<u32, u32>,
    entry: u32,
    end: u32,
    prologue_entry: u32,
) -> Option<Vec<SlotMap>> {
    let (entry, end) = (entry as usize, end as usize);
    let mut states: HashMap<usize, FrameState> = HashMap::new();
    let mut work = vec![entry];
    // Parameters are live words of unknown kind.
    let arity = entry_arity(bytecode, constants, entry, prologue_entry as usize)?;
    states.insert(
        entry,
        FrameState {
            lo: arity,
            hi: arity,
            bits: vec![true; arity],
            slot: vec![true; arity],
        },
    );
    let mut recorded: HashMap<usize, Vec<u16>> = HashMap::new();
    let mut steps = 0usize;
    while let Some(pc) = work.pop() {
        steps += 1;
        if steps > 200_000 {
            return None;
        }
        let mut st = states.get(&pc)?.clone();
        let b = bytecode.get(pc)?;
        let step = transfer(b, bytecode.get(pc + 1), constants, match_arities, &mut st, pc, end)?;
        if let Some(slots) = step.record.clone() {
            recorded.insert(pc, slots);
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
    let mut pcs: Vec<usize> = recorded.keys().copied().collect();
    pcs.sort_unstable();
    for pc in pcs {
        let mut st = states.get(&pc)?.clone();
        let step = transfer(&bytecode[pc], bytecode.get(pc + 1), constants, match_arities, &mut st, pc, end)?;
        out.push(SlotMap {
            pc: pc as u32,
            slots: step.record?,
        });
    }
    Some(out)
}

/// Parameter count of the body at `entry`, read from its callers' `CALL` /
/// `TailCall`s. Bodies entered any other way (jump, code pointer, coroutine,
/// host-only) or with differing arities are refused.
fn entry_arity(
    bytecode: &[Byte],
    constants: &[u64],
    entry: usize,
    prologue_entry: usize,
) -> Option<usize> {
    if entry == prologue_entry && entered_by_prologue_only(bytecode, constants, entry) {
        return Some(0);
    }
    let mut arity = None;
    for b in bytecode {
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
        // A plain code pointer is called with the declared arity; closures
        // (`MakePolyFn*`) append captures, jumps and coroutines do not match.
        if matches!(inst, Instruction::CodePtr) {
            continue;
        }
        if !matches!(inst, Instruction::CALL | Instruction::TailCall) {
            return None;
        }
        let a = b.call_parts().0;
        match arity {
            None => arity = Some(a),
            Some(prev) if prev == a => {}
            Some(_) => return None,
        }
    }
    arity
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
fn transfer(
    b: &Byte,
    tail_word: Option<&Byte>,
    constants: &[u64],
    match_arities: &HashMap<u32, u32>,
    st: &mut FrameState,
    pc: usize,
    end: usize,
) -> Option<Step> {
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
        CONST | CodePtr => st.push(false)?,
        LOAD => {
            for i in 0..b.load_store_count() {
                let slot = b.load_store_slot_at(i) as usize;
                let heap = st.bit(slot);
                st.push(heap)?;
            }
        }
        STORE | StorePop => {
            for i in 0..b.load_store_count() {
                let slot = b.load_store_slot_at(i) as usize;
                let heap = st.pop()?;
                st.store(slot, heap)?;
            }
        }
        Seek => st.seek(b.operand_u32() as usize),
        DUPLICATE => {
            let heap = st.pop()?;
            st.push(heap)?;
            st.push(heap)?;
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
        RETURN | LoadReturnSlot | ConstReturnImm | BinReturn | ReturnPair | TailCall
        | MakeEnumReturn | HALT | Panic => {
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
            for _ in 0..b.call_ret_words() {
                st.push(true)?;
            }
        }
        HostInvoke => {
            let arity = (b.operand_u32() & 0xFFFF) as usize;
            st.pop_n(arity + 1)?;
            st.push(true)?;
            step.record = Some(st.heap_slots(st.hi, true));
        }
        MakeTuple | MakeArray | MakeEnum => {
            st.pop_n((b.operand_u32() & 0xFFFF) as usize)?;
            st.push(true)?;
            step.record = Some(st.heap_slots(st.hi, true));
        }
        InitTyped | INIT | STRING => {
            st.push(true)?;
            step.record = Some(st.heap_slots(st.hi, true));
        }
        BoxValue | STRINGIFY => {
            st.pop()?;
            st.push(true)?;
            step.record = Some(st.heap_slots(st.hi, true));
        }
        FORMAT => {
            let n = b.operand_u32() as usize;
            if n != 0 {
                st.pop_n(n + 1)?;
                st.push(true)?;
            }
            step.record = Some(st.heap_slots(st.hi, true));
        }
        MakeDict => {
            st.pop_n(2 * (b.operand_u32() & 0xFFFF) as usize)?;
            st.push(true)?;
            step.record = Some(st.heap_slots(st.hi, true));
        }
        ArrayPush => {
            st.pop_n(2)?;
            st.push(true)?;
            step.record = Some(st.heap_slots(st.hi, true));
        }
        UnboxValue | LoadField => {
            st.pop()?;
            st.push(true)?;
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
        DenseConst => st.set(b.dense_const_parts().1, false)?,
        DenseArrayLen => st.set(b.dense_move_parts().0, false)?,
        DenseMove => {
            let (dest, src) = b.dense_move_parts();
            let heap = st.bit(src);
            st.set(dest, heap)?;
        }
        DenseIndex | DenseFieldLoad => st.set(b.dense_abc_parts().1, true)?,
        DenseStoreIndex | DenseFieldStore => {}
        DensePush => {
            let (arity, base) = b.dense_move_parts();
            for i in 0..arity {
                let heap = st.bit(base + i);
                st.push(heap)?;
            }
        }
        DenseMake | DenseArrayPush => {
            st.set(b.dense_abc_parts().1, true)?;
            step.record = Some(st.heap_slots(st.hi, true));
        }
        DenseMakeObject => {
            let (dest, _, _) = common::dense::unpack_make_object(b.operand_u32());
            st.set(dest as usize, true)?;
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
            st.pop_n(n)?;
            st.push(true)?;
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
        let mut code = vec![call(1, 2), op(Instruction::HALT)];
        code.extend_from_slice(body);
        let entries = vec![("f".to_string(), 2)];
        bind_precise_frames(&HashSet::new(), &code, &[], &HashMap::new(), &entries, u32::MAX)
    }

    fn slots_at(maps: &[PreciseFrameMap], pc: u32) -> Option<Vec<u16>> {
        precise_map_for_pc(maps, pc)?.slots_at_pc(pc).map(<[u16]>::to_vec)
    }

    use common::precise_map_for_pc;

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
            op(Instruction::VLoad),
            op(Instruction::RETURN),
        ]);
        assert!(maps.is_empty());
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
            bind_precise_frames(&HashSet::new(), &code, &[], &HashMap::new(), &entries, u32::MAX);
        assert!(maps.is_empty());
    }
}
