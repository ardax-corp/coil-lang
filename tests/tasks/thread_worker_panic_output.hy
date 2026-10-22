// #1828: a `thread::spawn` worker's panic message is not part of the
// case's stdout under `coil test`, as it is not under `coil`: it goes to
// stderr, so the `// Output:` header sees only what `main` printed.
// @output before\njoined: err\n
use io::{stdout, write};
use string::to_bytes;
use thread::{join, spawn};

fn dies() -> int {
    panic "worker died";
}

fn main() {
    let _ = write(stdout(), to_bytes("before\n"));
    let t = spawn(dies)?;
    if join(t).is_err() {
        let _ = write(stdout(), to_bytes("joined: err\n"));
    }
}
