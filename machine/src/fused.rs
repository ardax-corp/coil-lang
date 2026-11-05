//! Fused inner-op evaluation (same packed `u8` ISA discriminants as `Instruction`).
//!
//! rustc 1.98 has no stable guaranteed tail calls (`become` is nightly), so
//! token-threading the main loop is out. A 256-entry `fn` pointer table was
//! measured ~2% slower on mandelbrot (indirect call on the float fused path).
//! These helpers are `#[inline(always)]` matches so LLVM can emit a jump table
//! of straight-line arms without a second call.

use common::int_arith::IntTrap;
use common::{Instruction, Value};

use crate::arith::{self, };
use crate::{Heap, HeapSlot};

#[cfg(test)]
mod tests {
    use super::*;

}
