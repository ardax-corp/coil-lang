//! Local InstCombine / peephole on typed stack IL.
//!
//! Folds obvious adjacent patterns without new opcodes or ABI changes:
//! const-cond branches, known-tag `EQ`, XOR-1 pairs, two-slot enum
//! match diamonds that just keep the payload, and `Call; Return` → `TailCall`.

use common::{Byte, Instruction};

use super::super::op::{EntryKind, IlJumpKind, IlOp, Label};

/// Cheap local rewrites. Runs to a short fixpoint so const-`EQ` then
/// const-`JMPF` compose in one pipeline round.
pub(crate) fn instcombine(ops: &mut Vec<IlOp>) -> usize {
    if ops.len() < 2 {
        return 0;
    }
    let mut applied = 0usize;
    for _ in 0..8 {
        let n = instcombine_once(ops);
        if n == 0 {
            break;
        }
        applied += n;
    }
    applied
}

fn instcombine_once(ops: &mut Vec<IlOp>) -> usize {
    let mut out = Vec::with_capacity(ops.len());
    let mut i = 0;
    let mut hits = 0usize;
    while i < ops.len() {
        if let Some((consumed, rewrite)) = try_peep(ops, i) {
            out.extend(rewrite);
            i += consumed;
            hits += 1;
            continue;
        }
        out.push(ops[i].clone());
        i += 1;
    }
    if hits > 0 {
        *ops = out;
    }
    hits
}

fn try_peep(ops: &[IlOp], i: usize) -> Option<(usize, Vec<IlOp>)> {
    try_const_cond_jmp(ops, i)
        .or_else(|| try_known_tag_dup_eq(ops, i))
        .or_else(|| try_xor1_xor1(ops, i))
        .or_else(|| try_pair_payload_identity_match(ops, i))
        .or_else(|| try_call_return_tail(ops, i))
}

/// Adjacent `CALL` / `Entry{Call}` + matching `RETURN` → `TailCall`.
///
/// Same-function and known-sibling targets already carry a label; this is the
/// AOT jump rewrite (reuse frame, jump) without a new VM opcode. Refuses a
/// ret-width mismatch and fused `*Return` / `PairToHeap` (box-after-call).
fn try_call_return_tail(ops: &[IlOp], i: usize) -> Option<(usize, Vec<IlOp>)> {
    let ret_words = return_words(ops.get(i + 1)?)?;
    let tail = match &ops[i] {
        IlOp::Entry {
            kind: EntryKind::Call,
            arity,
            target,
            loc,
            ret_words: call_ret,
        } if *call_ret == ret_words => IlOp::Entry {
            kind: EntryKind::TailCall,
            arity: *arity,
            target: *target,
            loc: *loc,
            ret_words: 1,
        },
        IlOp::Byte { byte, loc } if *byte.bytecode() == Instruction::CALL => {
            let call_ret = byte.call_ret_words().max(1);
            if call_ret != ret_words {
                return None;
            }
            let (arity, target) = byte.call_parts();
            IlOp::Byte {
                byte: Byte::new(Instruction::TailCall)
                    .with_call_packed(arity as u32, target as u32),
                loc: *loc,
            }
        }
        _ => return None,
    };
    Some((2, vec![tail]))
}

fn return_words(op: &IlOp) -> Option<u32> {
    match op {
        IlOp::Return { ret_words, .. } => Some(*ret_words),
        IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::RETURN => {
            Some(byte.return_words().max(1))
        }
        _ => None,
    }
}

/// `CONST c; JMPF L` / `JMPT L` → unconditional jump or delete.
fn try_const_cond_jmp(ops: &[IlOp], i: usize) -> Option<(usize, Vec<IlOp>)> {
    let imm = match &ops[i] {
        IlOp::Const { imm, .. } => *imm,
        _ => return None,
    };
    let IlOp::Jump {
        kind,
        target,
        loc: jloc,
        ..
    } = ops.get(i + 1)?
    else {
        return None;
    };
    let taken = match kind {
        IlJumpKind::JumpIfFalse => imm == 0,
        IlJumpKind::JumpIfTrue => imm != 0,
        _ => return None,
    };
    if taken {
        Some((
            2,
            vec![IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: *target,
                loc: *jloc,
                hint: Default::default(),
            }],
        ))
    } else {
        Some((2, Vec::new()))
    }
}

/// `CONST t; DUP; CONST e; EQ|NEQ` → `CONST t; CONST (t ? e)`.
///
/// Pair construct + tag test: the tag stays under the compare for the
/// following `POP`.
fn try_known_tag_dup_eq(ops: &[IlOp], i: usize) -> Option<(usize, Vec<IlOp>)> {
    let (tag, loc) = match &ops[i] {
        IlOp::Const { imm, loc } => (*imm, *loc),
        _ => return None,
    };
    if !matches!(ops.get(i + 1)?, IlOp::Dup { .. }) {
        return None;
    }
    let expected = match ops.get(i + 2)? {
        IlOp::Const { imm, .. } => *imm,
        _ => return None,
    };
    let (op, bloc) = match ops.get(i + 3)? {
        IlOp::Bin { op, loc } => (*op, *loc),
        _ => return None,
    };
    let result = match op {
        Instruction::EQ => i32::from(tag == expected),
        Instruction::NEQ => i32::from(tag != expected),
        _ => return None,
    };
    Some((
        4,
        vec![
            IlOp::Const { imm: tag, loc },
            IlOp::Const {
                imm: result,
                loc: bloc,
            },
        ],
    ))
}

/// `…; CONST 1; XOR; CONST 1; XOR` → drop the two XORs (involution).
fn try_xor1_xor1(ops: &[IlOp], i: usize) -> Option<(usize, Vec<IlOp>)> {
    if !is_const_n(&ops[i], 1) {
        return None;
    }
    if !is_xor(ops.get(i + 1)?) {
        return None;
    }
    if !is_const_n(ops.get(i + 2)?, 1) {
        return None;
    }
    if !is_xor(ops.get(i + 3)?) {
        return None;
    }
    Some((4, Vec::new()))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ArmKind {
    /// `STORE s; LOAD s` — body is the bound payload.
    Payload,
    /// `POP; CONST 0` — unit variant dummy payload is ABI 0.
    UnitZero,
}

/// Two-slot match `DUP; CONST tag; EQ; JMPF miss; POP; arm; JMP end; miss: POP; arm; end:`.
///
/// Both arms yielding the payload (Result `Ok(v)|Err(e)` → that word), or one
/// payload arm plus a unit `=> 0` (Option `Some(x)|None => 0`), collapse to
/// `POP` of the tag.
fn try_pair_payload_identity_match(ops: &[IlOp], i: usize) -> Option<(usize, Vec<IlOp>)> {
    if !matches!(ops.get(i)?, IlOp::Dup { .. }) {
        return None;
    }
    if !matches!(ops.get(i + 1)?, IlOp::Const { .. }) {
        return None;
    }
    if !matches!(ops.get(i + 2)?, IlOp::Bin { op: Instruction::EQ, .. }) {
        return None;
    }
    let IlOp::Jump {
        kind: IlJumpKind::JumpIfFalse,
        target: miss,
        ..
    } = ops.get(i + 3)?
    else {
        return None;
    };
    if !matches!(ops.get(i + 4)?, IlOp::Pop { .. }) {
        return None;
    }
    let (hit_end, hit_kind) = consume_identity_arm(ops, i + 5)?;
    let IlOp::Jump {
        kind: IlJumpKind::Unconditional,
        target: end,
        loc: pop_loc,
        ..
    } = ops.get(hit_end)?
    else {
        return None;
    };
    let miss_lab = hit_end + 1;
    if !is_label(ops.get(miss_lab)?, miss.0) {
        return None;
    }
    if !matches!(ops.get(miss_lab + 1)?, IlOp::Pop { .. }) {
        return None;
    }
    let (miss_end, miss_kind) = consume_identity_arm(ops, miss_lab + 2)?;
    if !is_label(ops.get(miss_end)?, end.0) {
        return None;
    }
    if !arms_fold_to_payload(hit_kind, miss_kind) {
        return None;
    }
    let loc = match ops[i + 4] {
        IlOp::Pop { loc } => loc,
        _ => *pop_loc,
    };
    Some((
        miss_end + 1 - i,
        vec![
            IlOp::Pop { loc },
            ops[miss_lab].clone(),
            ops[miss_end].clone(),
        ],
    ))
}

fn consume_identity_arm(ops: &[IlOp], i: usize) -> Option<(usize, ArmKind)> {
    match (ops.get(i)?, ops.get(i + 1)?) {
        (IlOp::StorePop { slot: s0, .. }, IlOp::Load { slot: s1, .. }) if s0 == s1 => {
            Some((i + 2, ArmKind::Payload))
        }
        (IlOp::Pop { .. }, IlOp::Const { imm: 0, .. }) => Some((i + 2, ArmKind::UnitZero)),
        _ => None,
    }
}

fn arms_fold_to_payload(a: ArmKind, b: ArmKind) -> bool {
    matches!(
        (a, b),
        (ArmKind::Payload, ArmKind::Payload)
            | (ArmKind::Payload, ArmKind::UnitZero)
            | (ArmKind::UnitZero, ArmKind::Payload)
    )
}

fn is_label(op: &IlOp, id: u32) -> bool {
    matches!(op, IlOp::Label(Label(x)) | IlOp::JoinLabel(Label(x)) if *x == id)
}

fn is_const_n(op: &IlOp, n: i32) -> bool {
    matches!(op, IlOp::Const { imm, .. } if *imm == n)
}

fn is_xor(op: &IlOp) -> bool {
    matches!(op, IlOp::Bin { op: Instruction::XOR, .. })
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::DebugLoc;

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

    fn jmp(kind: IlJumpKind, id: u32) -> IlOp {
        IlOp::Jump {
            kind,
            target: Label(id),
            loc: loc(),
            hint: Default::default(),
        }
    }

    #[test]
    fn const_zero_jmpf_becomes_goto() {
        let mut ops = vec![
            IlOp::Const { imm: 0, loc: loc() },
            jmp(IlJumpKind::JumpIfFalse, 1),
            IlOp::Const { imm: 9, loc: loc() },
            IlOp::Label(Label(1)),
        ];
        assert!(instcombine(&mut ops) >= 1);
        assert!(matches!(
            ops[0],
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(1),
                ..
            }
        ));
    }

    #[test]
    fn const_nonzero_jmpf_is_deleted() {
        let mut ops = vec![
            IlOp::Const { imm: 1, loc: loc() },
            jmp(IlJumpKind::JumpIfFalse, 1),
            IlOp::Const { imm: 9, loc: loc() },
            IlOp::Label(Label(1)),
        ];
        instcombine(&mut ops);
        assert!(matches!(ops[0], IlOp::Const { imm: 9, .. }));
    }

    #[test]
    fn known_tag_dup_eq_folds_compare() {
        let mut ops = vec![
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::Dup { loc: loc() },
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::Bin {
                op: Instruction::EQ,
                loc: loc(),
            },
            jmp(IlJumpKind::JumpIfFalse, 1),
        ];
        instcombine(&mut ops);
        // tag stays; EQ true → JMPF deleted.
        assert!(matches!(ops[0], IlOp::Const { imm: 0, .. }));
        assert!(!ops
            .iter()
            .any(|op| matches!(op, IlOp::Jump { kind: IlJumpKind::JumpIfFalse, .. })));
    }

    #[test]
    fn xor1_twice_is_identity() {
        let mut ops = vec![
            IlOp::Load { slot: 2, loc: loc() },
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Bin {
                op: Instruction::XOR,
                loc: loc(),
            },
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Bin {
                op: Instruction::XOR,
                loc: loc(),
            },
            IlOp::Return {
                loc: loc(),
                ret_words: 1,
            },
        ];
        instcombine(&mut ops);
        assert_eq!(ops.len(), 2);
        assert!(matches!(ops[0], IlOp::Load { slot: 2, .. }));
        assert!(matches!(ops[1], IlOp::Return { .. }));
    }

    fn pair_identity_diamond(hit_payload: bool, miss_payload: bool) -> Vec<IlOp> {
        let mut ops = vec![
            IlOp::Dup { loc: loc() },
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::Bin {
                op: Instruction::EQ,
                loc: loc(),
            },
            jmp(IlJumpKind::JumpIfFalse, 1),
            IlOp::Pop { loc: loc() },
        ];
        push_arm(&mut ops, hit_payload, 3);
        ops.push(jmp(IlJumpKind::Unconditional, 2));
        ops.push(IlOp::Label(Label(1)));
        ops.push(IlOp::Pop { loc: loc() });
        push_arm(&mut ops, miss_payload, 4);
        ops.push(IlOp::Label(Label(2)));
        ops.push(IlOp::Return {
            loc: loc(),
            ret_words: 1,
        });
        ops
    }

    fn push_arm(ops: &mut Vec<IlOp>, payload: bool, slot: u32) {
        if payload {
            ops.push(IlOp::StorePop { slot, loc: loc() });
            ops.push(IlOp::Load { slot, loc: loc() });
        } else {
            ops.push(IlOp::Pop { loc: loc() });
            ops.push(IlOp::Const { imm: 0, loc: loc() });
        }
    }

    #[test]
    fn result_pair_match_both_payloads_pops_tag() {
        let mut ops = pair_identity_diamond(true, true);
        assert!(instcombine(&mut ops) >= 1);
        assert!(matches!(ops[0], IlOp::Pop { .. }));
        assert!(!ops.iter().any(|op| matches!(op, IlOp::Dup { .. })));
        assert!(!ops.iter().any(|op| matches!(
            op,
            IlOp::Bin {
                op: Instruction::EQ,
                ..
            }
        )));
        assert!(ops.iter().any(|op| matches!(op, IlOp::Return { .. })));
    }

    #[test]
    fn option_pair_match_some_or_zero_pops_tag() {
        let mut ops = pair_identity_diamond(true, false);
        instcombine(&mut ops);
        assert!(matches!(ops[0], IlOp::Pop { .. }));
        assert!(!ops.iter().any(|op| matches!(op, IlOp::StorePop { .. })));
    }

    #[test]
    fn pair_match_refuses_non_identity_body() {
        let mut ops = vec![
            IlOp::Dup { loc: loc() },
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::Bin {
                op: Instruction::EQ,
                loc: loc(),
            },
            jmp(IlJumpKind::JumpIfFalse, 1),
            IlOp::Pop { loc: loc() },
            IlOp::StorePop { slot: 3, loc: loc() },
            IlOp::Const { imm: 7, loc: loc() },
            jmp(IlJumpKind::Unconditional, 2),
            IlOp::Label(Label(1)),
            IlOp::Pop { loc: loc() },
            IlOp::StorePop { slot: 4, loc: loc() },
            IlOp::Load { slot: 4, loc: loc() },
            IlOp::Label(Label(2)),
        ];
        let before = ops.clone();
        instcombine(&mut ops);
        assert!(ops == before, "non-identity match must stay");
    }

    #[test]
    fn call_then_return_becomes_tail_call() {
        let mut ops = vec![
            IlOp::Entry {
                kind: EntryKind::Call,
                arity: 2,
                target: Label(7),
                loc: loc(),
                ret_words: 1,
            },
            IlOp::Return {
                loc: loc(),
                ret_words: 1,
            },
        ];
        assert!(instcombine(&mut ops) >= 1);
        assert_eq!(ops.len(), 1);
        assert!(matches!(
            ops[0],
            IlOp::Entry {
                kind: EntryKind::TailCall,
                arity: 2,
                target: Label(7),
                ..
            }
        ));
    }

    #[test]
    fn two_word_call_then_two_word_return_becomes_tail_call() {
        let mut ops = vec![
            IlOp::Entry {
                kind: EntryKind::Call,
                arity: 1,
                target: Label(3),
                loc: loc(),
                ret_words: 2,
            },
            IlOp::Return {
                loc: loc(),
                ret_words: 2,
            },
        ];
        assert!(instcombine(&mut ops) >= 1);
        assert!(matches!(
            ops[0],
            IlOp::Entry {
                kind: EntryKind::TailCall,
                ..
            }
        ));
    }

    #[test]
    fn call_return_width_mismatch_is_refused() {
        let mut ops = vec![
            IlOp::Entry {
                kind: EntryKind::Call,
                arity: 1,
                target: Label(3),
                loc: loc(),
                ret_words: 2,
            },
            IlOp::Return {
                loc: loc(),
                ret_words: 1,
            },
        ];
        let before = ops.clone();
        instcombine(&mut ops);
        assert!(ops == before, "width-mismatched Call;Return must stay");
    }
}
