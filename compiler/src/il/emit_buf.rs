//! Production emit sink: [`CodeBuf`] (and `&mut impl EmitBuf` helpers).

use common::{Byte, Instruction};

use super::CodeBuf;

/// Methods codegen still calls through `&mut impl EmitBuf` (or `CodeBuf` without
/// an inherent shadow). Typed `push_*` live on [`CodeBuf`]; this trait does not
/// re-export the rest as unused defaults.
pub trait EmitBuf {
    fn push_byte(&mut self, b: Byte);
    fn push(&mut self, b: Byte) {
        self.push_byte(b);
    }

    fn push_load(&mut self, slot: u32);
    fn push_make_enum(&mut self, tag: u16, arity: u16);
    fn push_box_value(&mut self, tag: u32);
    fn push_seek(&mut self, slot: u32);
    fn push_string(&mut self, idx: u32);
}

impl EmitBuf for CodeBuf {
    fn push_byte(&mut self, b: Byte) {
        self.push(b);
    }

    fn push_load(&mut self, slot: u32) {
        CodeBuf::push_load(self, slot);
    }

    fn push_make_enum(&mut self, tag: u16, arity: u16) {
        CodeBuf::push_make_enum(self, tag, arity);
    }

    fn push_box_value(&mut self, tag: u32) {
        CodeBuf::push_box_value(self, tag);
    }

    fn push_seek(&mut self, slot: u32) {
        self.push(Byte::new(Instruction::Seek).with_operand_u32(slot));
    }

    fn push_string(&mut self, idx: u32) {
        CodeBuf::push_string(self, idx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::il::IlOp;
    use common::Instruction;

    #[test]
    fn codebuf_emit_buf_trait_lifts_used_ops() {
        let mut buf = CodeBuf::new();
        EmitBuf::push_load(&mut buf, 1);
        EmitBuf::push_make_enum(&mut buf, 7, 1);
        EmitBuf::push_box_value(&mut buf, 3);
        EmitBuf::push_seek(&mut buf, 4);
        EmitBuf::push_string(&mut buf, 11);
        let ops = buf.ops();
        assert!(matches!(ops[0], IlOp::Load { slot: 1, .. }));
        assert!(matches!(
            ops[1],
            IlOp::MakeEnum {
                tag: 7,
                arity: 1,
                ..
            }
        ));
        assert!(matches!(ops[2], IlOp::BoxValue { tag: 3, .. }));
        assert!(matches!(
            ops[3],
            IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::Seek && byte.operand_u32() == 4
        ));
        assert!(matches!(ops[4], IlOp::String { idx: 11, .. }));
    }

    #[test]
    fn codebuf_emit_buf_push_packs_through_push_byte() {
        let mut buf = CodeBuf::new();
        EmitBuf::push(&mut buf, Byte::new(Instruction::PRINT));
        let ops = buf.ops();
        assert!(matches!(ops[0], IlOp::Print { .. }));
    }

    #[test]
    fn codebuf_push_byte_absorbs_residual_typed() {
        let mut buf = CodeBuf::new();
        buf.push(Byte::new(Instruction::CONST).with_const_pool(1));
        buf.push(Byte::new(Instruction::STRING).with_operand_u32(3));
        buf.push(Byte::new(Instruction::GetField));
        buf.push(Byte::new(Instruction::PRINT));
        buf.push(Byte::new(Instruction::HostInvoke).with_operand_u32(0));
        let ops = buf.ops();
        assert!(matches!(ops[0], IlOp::ConstPool { idx: 1, .. }));
        assert!(matches!(ops[1], IlOp::String { idx: 3, .. }));
        assert!(matches!(ops[2], IlOp::GetField { .. }));
        assert!(matches!(ops[3], IlOp::Print { .. }));
        assert!(matches!(ops[4], IlOp::HostInvoke { arity: 0, .. }));
    }
}
