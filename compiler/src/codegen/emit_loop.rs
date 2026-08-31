//! Loop jump helpers extracted from `do_compile` (stack-margin style).

use std::ops::Range;

use reporting::{ErrorCode, Label as DiagLabel, Message};

use super::{BbJumpKind, BbLabel, Compiler};

impl Compiler {
    pub(super) fn emit_loop_jump(
        &mut self,
        target: Option<BbLabel>,
        keyword: &str,
        range: Range<usize>,
    ) {
        if let (Some(label), Some(bb)) = (target, self.loop_bbs.last_mut()) {
            bb.emit_jump_to(label, BbJumpKind::Unconditional, self.bytecode.il_mut());
        } else {
            let mut message = Message::error(
                ErrorCode::CodegenError,
                format!("{keyword} outside of loop"),
                range.clone(),
            );
            message.push(DiagLabel::new(
                format!("`{keyword}` can only be used inside a loop"),
                range,
            ));
            self.messages.push(message);
        }
    }
}
