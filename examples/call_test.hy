// Phase 1.6 (Call lift) — CFG-path function-call test.
//
// Exercises the linearizer's new `Inst::Call` arm and the
// `is_straight_line` lift for `Call`. Both `add` and `main` are
// straight-line (no control flow, no Construct/
// Access, no Assignment/Defer), so both route through the
// `cfg_builder` + `linearize` pipeline.
//
// `add` is compiled via the CFG path; `main` calls `add` via
// the CFG path's CALL+JMP mechanism. The CALL instruction's
// `callee_name` is "add" (extracted by `cfg_builder::Expression::Call`
// from the `Identifier("add")` AST node). The linearizer looks
// "add" up in `Compiler::function_offsets` and patches the JMP's
// operand with the resolved offset.
//
// Expected output (one stdout write, no trailing newline): `"done"`.
// The return value of `add(3, 4)` is
// discarded (it's an expression statement with no consumer).

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn add(int a, int b) -> int {
    return a + b;
}

fn main() {
    add(3, 4);
    write_all(stdout(), to_bytes("done"));
}
