// Output: 1;2;2;3
// Calling a block lambda that returns `()` as a statement, directly and
// through a `() -> ()` parameter (#1214).
use io::{stdout, write};
use string::to_bytes;

fn apply(() -> () f) {
    f();
    f();
}

fn main() {
    let run = fn () {
        let n = 1;
        let _ = write(stdout(), to_bytes("1;"));
    };
    run();
    apply(fn () {
        let _ = write(stdout(), to_bytes("2;"));
    });
    let k = 3;
    let last = fn () use (k) {
        if k == 3 {
            let _ = write(stdout(), to_bytes("3"));
        }
    };
    last();
}
