// A method call on a short receiver whose lambda argument spans several
// lines keeps `s.run(fn () {` on one line; `coil fmt` used to put `s`,
// `.run(` and `fn () {` on three lines (#1774).

use tests::assert;

class Runner {
    pub total: int,
}

impl Runner {
    pub fn run(int -> int f) -> int {
        self.total = self.total + f(1);
        return self.total;
    }
}

fn with_runner(Runner -> int f) -> int {
    return f(new Runner(0));
}

test("a short receiver keeps its method and lambda on one line") {
    let r = with_runner(fn (Runner s) {
        s.run(fn (int x) {
            let n = 41 + x;
            let m = n * 2 - n;
            let k = m * 3 - m * 2;
            k
        });
        s.total
    });
    assert!(r == 42);
}

test("a short receiver in a statement keeps its lambda on the same line") {
    let s = new Runner(1);
    s.run(fn (int x) {
        let first_value_with_a_long_name = x + 1;
        let second_value_with_a_long_name = first_value_with_a_long_name * 2;
        second_value_with_a_long_name
    });
    assert!(s.total == 5);
}
