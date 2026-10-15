// `x++` / `--x` used as values in direct MIR: postfix gives the old word,
// prefix the new one, on int and float locals and statics alike.
use tests::assert;

static let ticks: int = 10;

static let level: float = 0.5;

fn locals(int n) -> int {
    let k = n;
    let a = k++;
    let b = ++k;
    let c = k--;
    let d = --k;
    return a * 1000 + b * 100 + c * 10 + d;
}

fn floats(float x) -> float {
    let f = x;
    let a = f++;
    let b = --f;
    return a + b * 10.0 + f * 100.0;
}

fn statics() -> int {
    let a = ticks++;
    let b = --ticks;
    let c = ticks--;
    return a * 100 + b * 10 + (c - ticks);
}

fn in_loop(int n) -> int {
    let i = 0;
    let s = 0;
    while i < n {
        s = s + i++;
    }
    return s * 10 + i;
}

test("adjust values on locals and statics") {
    assert!(locals(1) == 1331);
    assert!(floats(1.5) == 166.5);
    assert!(statics() == 1101);
    assert!(ticks == 9);
    let l = level++;
    assert!(l == 0.5);
    assert!(++level == 2.5);
    assert!(in_loop(4) == 64);
}
