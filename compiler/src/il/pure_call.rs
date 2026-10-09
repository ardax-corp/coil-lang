//! Pure-call context for IL passes that refuse impure `CALL` barriers (COI-99).
//!
//! Reuses the checker's `pure_fn_names` (auto-par's whole-function
//! purity). A callee is length-safe only when that set contains its bind name
//! (or a single-segment `mod::f` / `Type::m` suffix). Anything the lattice
//! cannot prove — host / FFI / `FORMAT`, field get/set, `CallIndirect`,
//! `ArrayPush` in the callee — stays a barrier.

use std::collections::{HashMap, HashSet};

use common::Instruction;

use super::effects::{Effects, effects};
use super::op::{EntryKind, IlOp, Label};

/// Maps entry labels and packed CALL offsets to callee names plus the AST purity set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PureCallCtx {
    pub pure_fns: HashSet<String>,
    pub label_callees: HashMap<u32, String>,
    /// Emit-time `CALL` targets (`self.functions` offsets) → bind names.
    pub offset_callees: HashMap<u32, String>,
    /// Callees that cannot change any array's length (superset of
    /// `pure_fns`); see [`crate::typechecking::purity::LengthStability`].
    pub length_stable_fns: HashSet<String>,
    /// No finalizer can change a length, so allocating ops (`FORMAT`, field
    /// key interning) are not length barriers.
    pub alloc_length_stable: bool,
}

impl PureCallCtx {
    pub fn call_is_pure(&self, target: Label) -> bool {
        self.label_callees
            .get(&target.0)
            .is_some_and(|n| self.name_is_pure(n))
    }

    pub fn call_offset_is_pure(&self, target: u32) -> bool {
        self.offset_callees
            .get(&target)
            .is_some_and(|n| self.name_is_pure(n))
    }

    fn name_is_pure(&self, name: &str) -> bool {
        name_in(&self.pure_fns, name)
    }

    /// True when `op` calls a user function that cannot change any array's
    /// length (whatever its return width).
    fn call_is_length_stable(&self, op: &IlOp) -> bool {
        let name = match op {
            IlOp::Entry {
                kind: EntryKind::Call,
                target,
                ..
            } => self.label_callees.get(&target.0),
            IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::CALL => {
                self.offset_callees.get(&(byte.call_parts().1 as u32))
            }
            _ => None,
        };
        name.is_some_and(|n| name_in(&self.length_stable_fns, n))
    }
}

/// Exact bind name, `$mono$` clone of a listed bind, or a single `::` suffix
/// against the AST short name.
pub(crate) fn name_in(set: &HashSet<String>, name: &str) -> bool {
    let stem = name.split("$mono$").next().unwrap_or(name);
    if set.contains(stem) {
        return true;
    }
    match stem.rsplit_once("::") {
        Some((prefix, short)) if !prefix.contains("::") => set.contains(short),
        _ => false,
    }
}

/// True when `op` blocks length-invariance / ArrayLen hoist for an array loop.
/// Array grow is handled per array by the callers; element stores are fine.
///
/// The question is only "can this change an array's length?", not "is this
/// pure?": a call to a length-stable user function passes even when it does
/// IO or writes fields. Field ops and `FORMAT` run no user code and never
/// resize, but they allocate, and allocation can run a finalizer — so they
/// pass only when no finalizer can resize.
pub fn op_blocks_length_proof(op: &IlOp, ctx: Option<&PureCallCtx>) -> bool {
    let mut e = effects(op, ctx);
    if e.any(Effects::CALL) && ctx.is_some_and(|c| c.call_is_length_stable(op)) {
        e = e.without(Effects::CALL);
    }
    if ctx.is_some_and(|c| c.alloc_length_stable) {
        e = e.without(Effects::FORMAT | Effects::FIELD_READ | Effects::FIELD_WRITE);
    }
    // Resume restores empty pin maps; pins are not saved on ObjCoroutine.
    e.any(
        Effects::CALL
            | Effects::HOST
            | Effects::FORMAT
            | Effects::FIELD_READ
            | Effects::FIELD_WRITE
            | Effects::YIELD
            | Effects::MATCH,
    )
}

#[cfg(test)]
mod tests {
    use common::{Byte, DebugLoc, Instruction};

    use super::*;

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

    #[test]
    fn pure_call_entry_is_not_a_length_barrier() {
        let mut ctx = PureCallCtx::default();
        ctx.pure_fns.insert("sq".into());
        ctx.label_callees.insert(7, "sq".into());
        let op = IlOp::Entry {
            kind: EntryKind::Call,
            arity: 1,
            target: Label(7),
            loc: loc(), ret_words: 1,};
        assert!(!op_blocks_length_proof(&op, Some(&ctx)));
    }

    #[test]
    fn impure_call_entry_stays_a_barrier() {
        let op = IlOp::Entry {
            kind: EntryKind::Call,
            arity: 1,
            target: Label(1),
            loc: loc(), ret_words: 1,};
        assert!(op_blocks_length_proof(&op, None));
    }

    #[test]
    fn pure_call_byte_offset_is_not_a_length_barrier() {
        let mut ctx = PureCallCtx::default();
        ctx.pure_fns.insert("sq".into());
        ctx.offset_callees.insert(42, "sq".into());
        let op = IlOp::Byte {
            byte: Byte::new(Instruction::CALL).with_call_packed(1, 42),
            loc: loc(),
        };
        assert!(!op_blocks_length_proof(&op, Some(&ctx)));
    }

    #[test]
    fn unknown_call_byte_stays_a_barrier() {
        let op = IlOp::Byte {
            byte: Byte::new(Instruction::CALL).with_call_packed(1, 42),
            loc: loc(),
        };
        assert!(op_blocks_length_proof(&op, None));
    }

    #[test]
    fn call_indirect_and_field_ops_stay_barriers() {
        assert!(op_blocks_length_proof(
            &IlOp::Byte {
                byte: Byte::new(Instruction::CallIndirect),
                loc: loc(),
            },
            None
        ));
        assert!(op_blocks_length_proof(&IlOp::GetField { loc: loc() }, None));
        assert!(op_blocks_length_proof(
            &IlOp::SetField {
                loc: loc(),
                index: None
            },
            None
        ));
    }

    #[test]
    fn yield_ops_are_length_proof_barriers() {
        assert!(op_blocks_length_proof(
            &IlOp::Byte {
                byte: Byte::new(Instruction::YieldCoro),
                loc: loc(),
            },
            None
        ));
        assert!(op_blocks_length_proof(
            &IlOp::Byte {
                byte: Byte::new(Instruction::YieldFromCoro),
                loc: loc(),
            },
            None
        ));
        assert!(op_blocks_length_proof(
            &IlOp::Byte {
                byte: Byte::new(Instruction::TailCall).with_call_packed(1, 0),
                loc: loc(),
            },
            None
        ));
    }

    #[test]
    fn two_purity_contexts_on_one_thread_do_not_mix() {
        let mut pure = PureCallCtx::default();
        pure.pure_fns.insert("sq".into());
        pure.label_callees.insert(7, "sq".into());
        let impure = PureCallCtx::default();
        let op = IlOp::Entry {
            kind: EntryKind::Call,
            arity: 1,
            target: Label(7),
            loc: loc(), ret_words: 1,};
        assert!(!op_blocks_length_proof(&op, Some(&pure)));
        assert!(op_blocks_length_proof(&op, Some(&impure)));
    }

    #[test]
    fn module_qualified_pure_name_matches_ast_short_name() {
        let mut ctx = PureCallCtx::default();
        ctx.pure_fns.insert("sq".into());
        ctx.label_callees.insert(3, "util::sq".into());
        assert!(ctx.call_is_pure(Label(3)));
        ctx.label_callees.insert(4, "mod::Type::sq".into());
        assert!(!ctx.call_is_pure(Label(4)));
        ctx.label_callees.insert(5, "sq$mono$3$0".into());
        assert!(ctx.call_is_pure(Label(5)));
    }

    /// Impure but length-stable: passes the length proof.
    #[test]
    fn length_stable_call_passes_length_proof_only() {
        let mut ctx = PureCallCtx::default();
        ctx.length_stable_fns.insert("absorb".into());
        ctx.label_callees.insert(9, "absorb".into());
        ctx.offset_callees.insert(40, "absorb".into());
        let entry = IlOp::Entry {
            kind: EntryKind::Call,
            arity: 2,
            target: Label(9),
            loc: loc(),
            ret_words: 1,
        };
        let byte = IlOp::Byte {
            byte: Byte::new(Instruction::CALL).with_call_packed(2, 40),
            loc: loc(),
        };
        for op in [&entry, &byte] {
            assert!(!op_blocks_length_proof(op, Some(&ctx)));
        }
        // A tail call to the same name is still a barrier.
        let tail = IlOp::Entry {
            kind: EntryKind::TailCall,
            arity: 2,
            target: Label(9),
            loc: loc(),
            ret_words: 1,
        };
        assert!(op_blocks_length_proof(&tail, Some(&ctx)));
    }

    /// Field ops and FORMAT allocate; a resizing finalizer keeps them barriers.
    #[test]
    fn field_and_format_pass_only_when_alloc_is_length_stable() {
        let get = IlOp::GetField { loc: loc() };
        let format = IlOp::Byte {
            byte: Byte::new(Instruction::FORMAT),
            loc: loc(),
        };
        let mut ctx = PureCallCtx::default();
        assert!(op_blocks_length_proof(&get, Some(&ctx)));
        assert!(op_blocks_length_proof(&format, Some(&ctx)));
        ctx.alloc_length_stable = true;
        assert!(!op_blocks_length_proof(&get, Some(&ctx)));
        assert!(!op_blocks_length_proof(&format, Some(&ctx)));
        let host = IlOp::HostInvoke { arity: 1, layout: 0, loc: loc() };
        assert!(op_blocks_length_proof(&host, Some(&ctx)));
    }
}
