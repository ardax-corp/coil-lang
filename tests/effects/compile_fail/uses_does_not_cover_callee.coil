// @expected E0413 — a declaration must cover its callees: `log` writes to a
// stream, which `uses {read}` does not cover.
use io::{stdout, write};
use string::to_bytes;

fn log(string s) {
    write(stdout(), to_bytes(s));
}

fn step(int x) -> int uses {read} {
    log("step");
    return x + 1;
}

fn main() {
    step(1);
}
