// Expected: E0102 — `write` takes a `Vec<byte>`; an unannotated `[1, 2]` is `[int; 2]`.
use io::{stdin, write};

fn main() {
    let data = [1, 2];
    write(stdin(), data);
}
