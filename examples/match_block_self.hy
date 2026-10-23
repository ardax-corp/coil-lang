// examples/match_block_self.hy — brace-block match arms with `self.method()`.
//
// Match arm bodies may be `{ … }` blocks. Those are expression blocks
// (not dict literals), so `self.get()` works inside them.
//
// @output 5

use io::sync::print;
use string::format;

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
    let _ = print(format("%i", c.describe()));
}
