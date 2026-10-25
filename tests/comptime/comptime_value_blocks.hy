// @flags --comptime-read tests/comptime/data
// `comptime { … }` in an expression: the block runs while compiling, and the
// value it returns takes its place, like a `comptime` expression.
use io::file::read_text;
use text::Text;
use tests::assert;

fn sq(int x) -> int {
    return x * x;
}

class Limits {
    pub low: int,
    pub high: int,
}

test("a block calls this module's functions") {
    let n = comptime {
        let base = sq(3);
        let extra = comptime sq(2);
        return base * 4 + extra;
    };
    assert!(n == 40);
}

test("a block reads a file") {
    let count = comptime {
        let text = read_text("tests/comptime/data/names.txt") ?? "";
        let names: Vec<string> = text.lines();
        let n = 0;
        for name in names {
            if name != "" {
                n += 1;
            }
        }
        return n;
    };
    assert!(count == 2);
}

test("a block builds a class value") {
    let limits = comptime {
        let low = sq(2);
        return new Limits(low, low * 10);
    };
    assert!(limits.low == 4 && limits.high == 40);
}
