// examples/match_block_self.hy — brace-block match arms with `self.method()`.
//
// Match arm bodies may be `{ … }` blocks. Those are expression blocks
// (not dict literals), so `self.get()` works inside them.
//
// Expected output: `5`

use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};
enum Mode {
    Zero,
    Other(int),
}

class Counter {
    pub value: int,
}

impl Counter {
    pub fn get() -> int {
        return self.value;
    }

    pub fn describe() -> int {
        let m = Mode::Other(self.value);
        return match m {
            Mode::Zero => {
                self.get()
            },
            Mode::Other(n) => {
                self.get();
                n
            },
        };
    }
}

fn main() {
    let c = new Counter(5);
    write_all(stdout(), to_bytes(format("%i", c.describe())));
}
