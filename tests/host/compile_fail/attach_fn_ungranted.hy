// Expected: E0408 — the free function `io::attach` requires `--allow-attach`.
use io::{stdout, attach};

fn main() {
    let s = stdout();
    let _ = attach(s, 0, 0, 0, 0, 0);
}
