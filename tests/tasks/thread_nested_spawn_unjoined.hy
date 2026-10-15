// A thread spawned by a thread is joined at exit too: `main` and `mid`
// return without `join`, and `leaf` still runs.
// Output: leaf
use thread::spawn;
use io::{stdout, write};
use string::{format, to_bytes};

fn leaf() {
    write(stdout(), to_bytes("leaf"));
    return 0;
}

fn mid() {
    let _ = spawn(leaf)?;
    return 0;
}

fn main() {
    let _ = spawn(mid)?;
}
