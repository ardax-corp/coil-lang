//! Pipeline round behaviour and opt stats.

use super::*;
use crate::il::op::{IlJumpKind, IlOp, Label};
use common::DebugLoc;

fn loc() -> DebugLoc {
    DebugLoc::unknown()
}

fn label(id: u32) -> IlOp {
    IlOp::Label(Label(id))
}

fn ret() -> IlOp {
    IlOp::Return { loc: loc(), ret_words: 1}
}

fn c(n: i32) -> IlOp {
    IlOp::Const {
        imm: n,
        loc: loc(),
    }
}
