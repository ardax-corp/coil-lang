// @output main;dropped
// A finalizer still pending when `main` returns runs before exit, and the
// harness sees its output just as `coil FILE` prints it (#1212).
use io::stdout;
use io::sync::write_all;
use string::to_bytes;

class Noisy {
    pub n: int,
}

impl Noisy {
    fn drop() {
        let _ = write_all(stdout(), to_bytes("dropped"));
    }
}

static let kept: Option<Noisy> = Option::None;

fn main() {
    kept = Option::Some(new Noisy(1));
    let _ = write_all(stdout(), to_bytes("main;"));
}
