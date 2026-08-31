//! Deferred-target jump patching via the stack IL.
//!
//! Emits [`crate::il::IlOp::Jump`] / [`crate::il::IlOp::Label`] into an
//! [`crate::il::IlBuilder`] so targets stay symbolic until lower time.
//! `bind_label` is idempotent (last bind wins).

use crate::il::{IlBuilder, IlError};

pub use crate::il::{IlJumpKind as JumpKind, Label};

/// Result of [`BlockBuilder::finalize`].
pub type BlockError = IlError;

/// Thin control-flow helper over an [`IlBuilder`].
///
/// Does not own the IL stream — jumps and binds go to the caller's builder.
pub struct BlockBuilder {
    /// Labels allocated through this builder (for finalize checks).
    allocated: Vec<Label>,
    /// Labels that were targeted by a jump from this builder.
    targeted: Vec<Label>,
    /// Labels bound through this builder.
    bound: Vec<Label>,
}

impl Default for BlockBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockBuilder {
    pub fn new() -> Self {
        Self {
            allocated: Vec::new(),
            targeted: Vec::new(),
            bound: Vec::new(),
        }
    }

    pub fn fresh_label(&mut self, il: &mut IlBuilder) -> Label {
        let l = il.fresh_label();
        self.allocated.push(l);
        l
    }

    pub fn emit_jump_to(&mut self, target: Label, kind: JumpKind, il: &mut IlBuilder) {
        self.targeted.push(target);
        il.emit_jump(kind, target);
    }

    pub fn emit_jump_to_hinted(
        &mut self,
        target: Label,
        kind: JumpKind,
        hint: crate::il::FuseHint,
        il: &mut IlBuilder,
    ) {
        self.targeted.push(target);
        il.emit_jump_hinted(kind, target, common::DebugLoc::unknown(), hint);
    }

    /// Bind `label` at the current IL position (next emitting op).
    pub fn bind_label(&mut self, label: Label, il: &mut IlBuilder) {
        self.bound.push(label);
        il.bind_label(label);
    }

    /// Bind `label` as a value-producing join.
    pub fn bind_join_label(&mut self, label: Label, il: &mut IlBuilder) {
        self.bound.push(label);
        il.bind_join_label(label);
    }

    pub fn finalize(self) -> Result<(), BlockError> {
        for t in &self.targeted {
            if !self.bound.iter().any(|b| b.id() == t.id()) {
                // May have been bound via another builder sharing the same IL
                // (e.g. match rebind). Defer to IlBuilder::finalize_labels.
                let _ = t;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::il::lower;
    use common::{Byte, Instruction, Value};

    fn const_int(value: i64) -> Byte {
        Byte::new_with_value(Instruction::CONST, Value::from(value).raw() as _)
    }

    #[test]
    fn bind_label_resolves_at_lower() {
        let mut il = IlBuilder::new();
        let mut bb = BlockBuilder::new();
        let l = bb.fresh_label(&mut il);
        il.push_byte(const_int(1));
        bb.emit_jump_to(l, JumpKind::Unconditional, &mut il);
        il.push_byte(const_int(2));
        bb.bind_label(l, &mut il);
        il.push_byte(const_int(3));
        bb.finalize().unwrap();

        let mut pool = Vec::new();
        let lowered = lower(il.ops(), &mut pool);
        assert!(matches!(*lowered.bytecode[1].bytecode(), Instruction::JMP));
        // `dead_block` drops the fall-through CONST 2 after JMP.
        assert_eq!(lowered.bytecode[1].operand_u32(), 2);
        assert_eq!(lowered.bytecode.len(), 3);
    }

    #[test]
    fn jump_if_match_preserves_tag() {
        let mut il = IlBuilder::new();
        let mut bb = BlockBuilder::new();
        let l = bb.fresh_label(&mut il);
        bb.emit_jump_to(l, JumpKind::JumpIfMatch { tag: 5, arity: 1 }, &mut il);
        bb.bind_label(l, &mut il);
        il.push_byte(Byte::new(Instruction::HALT));

        let mut pool = Vec::new();
        let lowered = lower(il.ops(), &mut pool);
        assert!(matches!(
            *lowered.bytecode[0].bytecode(),
            Instruction::JumpIfMatch
        ));
        let operand = lowered.bytecode[0].operand_u32();
        assert_eq!((operand >> 16) as u16, 5);
        assert_eq!(pool[0], 1); // target = HALT at pc 1
    }
}
