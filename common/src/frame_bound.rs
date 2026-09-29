//! Static bound on how far one frame can raise the operand-stack cursor.
//!
//! The VM keeps [`FrameReserve::words`] free above the cursor for every frame
//! it opens, and grows the stack when it does not fit. Recursion depth then
//! only costs memory; it can never write past the buffer.
//!
//! Code is split into components linked by fall-through and jumps (calls and
//! code pointers open new frames, so they do not link). Within a component
//! the cursor stays below `max slot touched + sum of per-op pushes`: loops
//! have no net push, and `STORE` / `Seek` / dense registers only raise the
//! cursor to a slot counted in the first term.

use crate::{Byte, Instruction};

/// What the VM keeps free for the frames of one program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameReserve {
    /// Words a frame can occupy above the base its caller gives it,
    /// including its arguments (the widest `CALL` / `TailCall` arity). Never
    /// less than one.
    pub words: usize,
    /// Widest enum payload the program builds. A `JumpIfMatch` hit is
    /// counted as pushing this many words; a wider (host-built) payload
    /// must re-check the stack.
    pub match_payload: usize,
}

/// [`FrameReserve`] for every frame `code` can run.
#[must_use]
pub fn frame_reserve(code: &[Byte], pool: &[u64]) -> FrameReserve {
    let match_payload = widest_payload(code, pool);
    let args = code
        .iter()
        .filter(|b| matches!(*b.bytecode(), Instruction::CALL | Instruction::TailCall))
        .map(|b| b.call_parts().0)
        .max()
        .unwrap_or(0);
    let words = frame_words(code, pool, match_payload)
        .saturating_add(args)
        .max(1);
    FrameReserve {
        words,
        match_payload,
    }
}

/// Host natives build `Option` / `Result` payloads of one word.
const HOST_PAYLOAD: usize = 2;

fn widest_payload(code: &[Byte], pool: &[u64]) -> usize {
    code.iter()
        .filter_map(|b| match *b.bytecode() {
            Instruction::MakeEnum
            | Instruction::MakeEnumReturn
            | Instruction::MakeEnumK
            | Instruction::MakeEnumReturnK => Some(b.make_arity() as usize),
            Instruction::DenseMake if b.dense_abc_parts().0 >= crate::dense::MAKE_ENUM => {
                Some(b.dense_abc_parts().2)
            }
            Instruction::DenseMakeK => b
                .dense_make_k_parts(pool)
                .filter(|p| p.0 >= crate::dense::MAKE_ENUM)
                .map(|p| p.2),
            _ => None,
        })
        .max()
        .unwrap_or(0)
        .max(HOST_PAYLOAD)
}

fn frame_words(code: &[Byte], pool: &[u64], match_payload: usize) -> usize {
    let n = code.len();
    let steps: Vec<Step> = code.iter().map(|b| step(b, pool, match_payload)).collect();
    let mut parent: Vec<usize> = (0..n).collect();
    for (pc, s) in steps.iter().enumerate() {
        if s.falls && pc + 1 < n {
            union(&mut parent, pc, pc + 1);
        }
        match s.jump {
            Jump::To(t) if t < n => union(&mut parent, pc, t),
            Jump::To(_) | Jump::None => {}
            Jump::Unknown => return whole_program(&steps),
        }
    }
    let mut per_root: Vec<Bound> = vec![Bound::default(); n];
    for (pc, s) in steps.iter().enumerate() {
        let root = find(&mut parent, pc);
        per_root[root].add(s);
    }
    per_root.iter().map(Bound::words).max().unwrap_or(0)
}

fn whole_program(steps: &[Step]) -> usize {
    let mut all = Bound::default();
    steps.iter().for_each(|s| all.add(s));
    all.words()
}

#[derive(Clone, Copy, Default)]
struct Bound {
    push: u64,
    slots: u64,
}

impl Bound {
    fn add(&mut self, s: &Step) {
        self.push = self.push.saturating_add(s.push);
        self.slots = self.slots.max(s.slots);
    }

    fn words(&self) -> usize {
        usize::try_from(self.push.saturating_add(self.slots)).unwrap_or(usize::MAX)
    }
}

fn find(parent: &mut [usize], mut x: usize) -> usize {
    while parent[x] != x {
        parent[x] = parent[parent[x]];
        x = parent[x];
    }
    x
}

fn union(parent: &mut [usize], a: usize, b: usize) {
    let (ra, rb) = (find(parent, a), find(parent, b));
    if ra != rb {
        parent[ra.max(rb)] = ra.min(rb);
    }
}

enum Jump {
    None,
    To(usize),
    /// A branch whose target cannot be decoded: bound the whole program.
    Unknown,
}

struct Step {
    /// Most words the op can leave above the cursor it started at.
    push: u64,
    /// Highest frame slot the op names, plus one.
    slots: u64,
    jump: Jump,
    falls: bool,
}

fn step(b: &Byte, pool: &[u64], match_payload: usize) -> Step {
    use Instruction::*;
    let at = |i: usize| pool.get(i).copied();
    let hi = |xs: &[usize]| xs.iter().max().map_or(0, |m| *m as u64 + 1);
    let mut s = Step {
        push: 0,
        slots: 0,
        jump: Jump::None,
        falls: true,
    };
    let pool_target =
        |entry: Option<u64>| entry.map_or(Jump::Unknown, |v| Jump::To((v >> 32) as usize));
    let direct_or_pool = |t: usize, is_pool: bool| {
        if is_pool {
            at(t).map_or(Jump::Unknown, |v| Jump::To(v as usize))
        } else {
            Jump::To(t)
        }
    };
    match *b.bytecode() {
        // Leave the frame, or abort the VM (retired and unhandled opcodes panic).
        HALT | RETURN | LoadReturnSlot | ConstReturnImm | BinReturn | ReturnPair
        | MakeEnumReturn | MakeEnumReturnK | TailCall | Panic | SET | OptionNicheToHeap
        | HeapOptionToNiche
        | PairJumpIfTag | PairToHeap | HeapToPair | HostInvokeNiche | FloatChainStore
        | BinSlotSlotConstJmpf => {
            s.falls = false;
            match *b.bytecode() {
                LoadReturnSlot => s.slots = hi(&[b.operand_u32() as usize]),
                TailCall => s.slots = hi(&[b.call_parts().0]),
                _ => {}
            }
        }
        JMP => {
            s.falls = false;
            s.jump = Jump::To(b.operand_u32() as usize);
        }
        JMPF | JMPT => s.jump = Jump::To(b.operand_u32() as usize),
        CmpJmpf | CmpJmpt => {
            s.jump = direct_or_pool(b.cmp_jmpf_parts().1, b.cmp_jmpf_is_pool());
        }
        LogNotJmpf | LogNotJmpt => {
            s.jump = direct_or_pool(b.log_not_jmpf_target(), b.log_not_jmpf_is_pool());
        }
        BinSlotImmJmpf | BinSlotImmJmpt => {
            let (_, slot, idx) = b.bin_slot_imm_jmpf_parts();
            s.slots = hi(&[slot]);
            s.jump = pool_target(at(idx));
        }
        BinSlotSlotJmpf | BinSlotSlotJmpt => {
            let (_, a, idx) = b.bin_slot_slot_jmpf_parts();
            let other = at(idx).map_or(0, |v| (v & 0xFF) as usize);
            s.slots = hi(&[a, other]);
            s.jump = pool_target(at(idx));
        }
        BinSlotSlotConstJmpt => {
            let (_, a, idx) = b.bin_slot_slot_const_jmpf_parts();
            let other = at(idx).map_or(0, |v| (v & 0xFF) as usize);
            s.slots = hi(&[a, other]);
            s.jump = pool_target(at(idx));
        }
        // The hit edge pops the scrutinee and pushes its payload.
        JumpIfMatch => {
            s.push = match_payload as u64;
            s.jump = at((b.operand_u32() & 0xFFFF) as usize)
                .map_or(Jump::Unknown, |t| Jump::To(t as usize));
        }
        // A callee frame is checked when it opens; the caller keeps its result.
        CALL | CallIndirect => s.push = 2,
        MakeCoro | ResumeCoro | YieldFromCoro => s.push = 1,
        LOAD => {
            let n = b.load_store_count();
            s.push = n as u64;
            s.slots = hi(&(0..n)
                .map(|i| b.load_store_slot_at(i) as usize)
                .collect::<Vec<_>>());
        }
        STORE | StorePop => {
            let n = b.load_store_count();
            s.slots = hi(&(0..n)
                .map(|i| b.load_store_slot_at(i) as usize)
                .collect::<Vec<_>>());
        }
        Seek => s.slots = u64::from(b.operand_u32()),
        INC | DEC => {
            s.push = 1;
            s.slots = hi(&[b.inc_dec_parts().0]);
        }
        BinSlotImm => {
            s.push = 1;
            s.slots = hi(&[b.bin_slot_imm_parts().1]);
        }
        BinSlotSlot => {
            let (_, a, c) = b.bin_slot_slot_parts();
            s.push = 1;
            s.slots = hi(&[a, c]);
        }
        BinSlotImmStore => {
            let (_, src, idx) = b.bin_slot_imm_store_parts();
            let dest = at(idx).map_or(0, |v| (v >> 32) as usize);
            s.slots = hi(&[src, dest]);
        }
        BinSlotSlotStore => {
            let (_, a, c, dest) = b.bin_slot_slot_store_parts();
            s.slots = hi(&[a, c, dest]);
        }
        UnpackAt => {
            let op = b.operand_u32();
            s.slots = u64::from(op & 0xFFFF) + u64::from(op >> 16);
        }
        Unpack => s.push = u64::from(b.operand_u32()),
        ArrayPin | IndexPin | IndexPinUnchecked | StoreIndexPin | StoreIndexPinUnchecked => {
            s.slots = hi(&[b.operand_u32() as usize]);
        }
        DenseBin | DenseBin2 | DenseBinJmpf | DenseCmp | DenseIndex | DenseIndexJmpf
        | DenseStoreIndex | DenseFieldLoad | DenseFieldStore | DenseArrayPush => {
            let (_, d, x, y) = b.dense_abc_parts();
            s.slots = hi(&[d, x, y]);
        }
        DenseMake => {
            let (_, dest, arity, base) = b.dense_abc_parts();
            s.slots = hi(&[dest, base + arity.max(1) - 1]);
        }
        DenseMakeK => {
            if let Some((_, dest, arity, base, _)) = b.dense_make_k_parts(pool) {
                s.slots = hi(&[dest, base + arity.max(1) - 1]);
            }
        }
        DenseMakeObject => {
            s.slots = hi(&[crate::dense::unpack_make_object(b.operand_u32()).0 as usize]);
        }
        DenseConst => s.slots = hi(&[b.dense_const_parts().1]),
        DenseMove | DenseArrayLen => {
            let (d, x) = b.dense_move_parts();
            s.slots = hi(&[d, x]);
        }
        DenseUnary | DenseCast => {
            let (_, d, x) = b.dense_unary_parts();
            s.slots = hi(&[d, x]);
        }
        DensePush => {
            let (arity, base) = b.dense_move_parts();
            s.push = arity as u64;
            s.slots = (base + arity) as u64;
        }
        VLoad | VStore => {
            let (_, _, arr, idx) = b.dense_abc_parts();
            s.slots = hi(&[arr, idx]);
        }
        VBin => s.slots = hi(&[b.dense_abc_parts().2]),
        VReduce => s.slots = hi(&[b.dense_abc_parts().1]),
        VMove | VFma => {}
        // One new word at most (pops, if any, come first).
        DUPLICATE | CONST | STRING | CodePtr | INIT | InitTyped | MakeEnum | MakeEnumK
        | MakeTuple | MakeTupleK
        | MakeArray | MakeDict | MakePolyFn | MakePolyFnCapture | MakeFn | LoadStatic | FfiLoad
        | FfiInvoke | DeclareFFI | HostInvoke => s.push = 1,
        // Net pops or in place.
        NOOP | DATA | NATIVE | POP | ADD | SUB | MUL | DIV | MOD | ADDF | SUBF | MULF | DIVF
        | MODF | NOT | NEG | NEGF | AND | OR | SHL | SHR | XOR | EQ | NEQ | LE | LEQ | LEF
        | LEQF | GT | GEQ | GTF | GEQF | Pow | PowF | BITAND | BITOR | LogNot | PRINT | FORMAT
        | STRINGIFY | LoadField | Index | IndexUnchecked | StoreIndex | StoreIndexUnchecked
        | GetField | SetField | YieldCoro | DoneCoro | ArrayPush | ArrayLen | BoxValue
        | UnboxValue | DynAdd | DynSub | DynMul | DynDiv | DynMod | DynCmp | DynEq | DynNe
        | DynPrint | DictEntries | StoreStatic | CastIntToFloat | CastFloatToInt
        | CastIntToByte | CastByteToInt | CastIntToBool | CastBoolToInt | TagEnumType => {}
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(i: Instruction) -> Byte {
        Byte::new(i)
    }

    fn words(code: &[Byte]) -> usize {
        frame_reserve(code, &[]).words
    }

    fn konst(v: i32) -> Byte {
        Byte::new(Instruction::CONST).with_const_inline(v)
    }

    #[test]
    fn straight_line_counts_every_push() {
        let code = [
            konst(1),
            konst(2),
            konst(3),
            op(Instruction::MakeArray).with_operand_u32(3),
            op(Instruction::RETURN),
        ];
        assert_eq!(words(&code), 4);
    }

    #[test]
    fn a_call_target_is_its_own_frame() {
        let callee = 4;
        let code = [
            konst(1),
            Byte::new(Instruction::CALL).with_call_packed(1, callee),
            op(Instruction::RETURN),
            op(Instruction::HALT),
            konst(1),
            konst(2),
            konst(3),
            op(Instruction::RETURN),
        ];
        // Caller: 1 + 2 (call result); callee: 3 pushes; plus one argument.
        assert_eq!(words(&code), 4);
    }

    #[test]
    fn a_jump_joins_the_code_it_reaches() {
        let code = [
            konst(1),
            op(Instruction::JMP).with_operand_u32(3),
            op(Instruction::HALT),
            konst(2),
            konst(3),
            op(Instruction::RETURN),
        ];
        assert_eq!(words(&code), 3);
    }

    #[test]
    fn stores_and_seeks_count_their_slot() {
        let code = [
            konst(1),
            Byte::new(Instruction::STORE).with_load_store_slot(40),
            op(Instruction::Seek).with_operand_u32(12),
            op(Instruction::RETURN),
        ];
        assert_eq!(words(&code), 41 + 1);
    }

    #[test]
    fn an_undecodable_branch_bounds_the_whole_program() {
        let code = [
            konst(1),
            op(Instruction::HALT),
            konst(2),
            Byte::new(Instruction::BinSlotImmJmpf).with_bin_slot_imm_jmpf(
                Instruction::LE as u8,
                0,
                9,
            ),
            op(Instruction::RETURN),
        ];
        assert_eq!(words(&code), 3);
    }

    #[test]
    fn empty_code_still_reserves_a_word() {
        assert_eq!(words(&[]), 1);
    }

    #[test]
    fn a_match_hit_counts_the_widest_payload() {
        let code = [
            konst(1),
            konst(2),
            konst(3),
            op(Instruction::MakeEnum).with_operands_u16([0, 3]),
            op(Instruction::JumpIfMatch).with_operand_u32(0),
            op(Instruction::RETURN),
        ];
        let reserve = frame_reserve(&code, &[5]);
        assert_eq!(reserve.match_payload, 3);
        assert_eq!(reserve.words, 3 + 1 + 3);
    }
}
