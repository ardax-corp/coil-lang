// A module `const` and a local `const` can hold the `stdout()` stream and be
// written through (#1709): a `Stream` is an opaque handle, so `const` of one
// is fully constant and does not warn E0119 (see compiler/tests/diagnostics.rs).
// Output: ab
use io::{stdout, write};
use string::to_bytes;

const OUT = stdout();

fn main() {
    const out = stdout();
    write(OUT, to_bytes("a"));
    write(out, to_bytes("b"));
}
