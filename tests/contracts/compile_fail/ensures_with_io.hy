// @expected E0413 — an `ensures` clause must have no effects; `say` writes to stdout.
use io::stdout;
use io::sync::write_all;
use string::to_bytes;

fn say() -> bool {
    write_all(stdout(), to_bytes("x"));
    return true;
}

fn f(int x) -> int
    ensures say()
{
    return x;
}

fn main() {
    f(1);
}
