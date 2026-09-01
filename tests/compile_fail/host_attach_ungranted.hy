// Expected: compile failure — `Stream.attach` requires `--allow-attach`.
use io::{stdout};

fn main() {
    let s = stdout();
    let _ = s.attach(0, 0, 0, 0, 0);
}
