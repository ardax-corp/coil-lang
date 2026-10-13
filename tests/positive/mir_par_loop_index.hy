// A dynamic parallel `while` whose index is read after the loop: the index
// ends at the larger of its start and the loop's end, also when the loop
// runs no trips.
use tests::assert;

fn upto(int n) -> int {
    let acc = 0;
    let i = 0;
    while i < n {
        acc = acc + i % 4;
        i = i + 1;
    }
    return acc * 10 + i;
}

fn from_to(int b, int n) -> int {
    let acc = 0;
    let i = b;
    while i < n {
        acc = acc + i % 3;
        i = i + 1;
    }
    return acc * 1000000 + i;
}

fn through(int n) -> int {
    let acc = 0;
    let i = 0;
    while i <= n {
        acc = acc + i % 2;
        i = i + 1;
    }
    return acc * 1000000 + i;
}

test("index after a dynamic end") {
    assert!(upto(80000) == 20000 * 6 * 10 + 80000);
    assert!(upto(3) == 33);
    assert!(upto(0) == 0);
    assert!(upto(0 - 5) == 0);
}

test("index after a dynamic start") {
    // 3..60003 is 20000 cycles of 0, 1, 2.
    assert!(from_to(3, 60003) == 20000 * 3 * 1000000 + 60003);
    assert!(from_to(7, 2) == 7);
    assert!(from_to(5, 5) == 5);
}

test("index after an inclusive end") {
    // 0..=59999 holds 30000 odd numbers.
    assert!(through(59999) == 30000 * 1000000 + 60000);
    assert!(through(0 - 1) == 0);
}
