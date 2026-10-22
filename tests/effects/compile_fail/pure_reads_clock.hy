// @expected E0413 — reading the clock is the `read` effect.
use clock::mono_nanos;

pure fn stamp() -> int {
    return mono_nanos();
}

fn main() {
    stamp();
}
