// #1828: what a case and its workers write to `stderr()` is not part of the
// case's stdout under `coil test`, so the `// Output:` header sees only
// what went to `stdout()`.
// @output main out\nworker out\n
use io::{stderr, stdout, write};
use string::to_bytes;
use thread::{join, spawn};

fn worker() -> int {
    let _ = write(stderr(), to_bytes("worker err\n"));
    let _ = write(stdout(), to_bytes("worker out\n"));
    return 1;
}

fn main() {
    let _ = write(stderr(), to_bytes("main err\n"));
    let _ = write(stdout(), to_bytes("main out\n"));
    let t = spawn(worker)?;
    let _ = join(t)?;
}
