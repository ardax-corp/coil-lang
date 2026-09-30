// examples/let_test.hy — let-bound variable binding and re-assignment.
//
// This example exercises the Phase 18E `let x = expr;` codegen
// fix. The pre-18E codegen never explicitly wrote the RHS value
// into the let-bound slot, so the simple case `let x = 5;` worked
// by coincidence (slot 0 coincided with the operand-stack top).
// Re-assignment via `x = 10;` was broken because the codegen
// emitted `STORE` (a no-op since Phase 15D) + a buggy
// `DUPLICATE`.
//
// Phase 18E: the codegen special-cases the
// `[Variable(x), rhs]` Fragment shape and emits
// `STORE_POP slot` after the RHS. Re-assignment uses the same
// `STORE_POP` opcode directly.
//
// Expected output: "51020" (5, then 10, then 20 after re-assignment).
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    let x = 5;
    write_all(stdout(), to_bytes(format("%i", x)));
    let y = 10;
    write_all(stdout(), to_bytes(format("%i", y)));
    x = 20;
    write_all(stdout(), to_bytes(format("%i", x)));
}
