// The fork-join function of an auto-parallel loop is built as HIR and
// lowered to direct MIR: dynamic begin and end, an inclusive end, a
// stride, a range at or under the grain (run on this thread) and a
// product reduction.
use tests::assert;

// Dynamic begin and end.
fn span(int lo, int hi) -> int {
    let acc = 0;
    for x in lo..hi {
        acc = acc + x % 7;
    }
    return acc;
}

// Inclusive end: the end bound is biased by one.
fn span_incl(int lo, int hi) -> int {
    let acc = 0;
    for x in lo..=hi {
        acc = acc + x % 5;
    }
    return acc;
}

// A stride of two.
fn evens(int n) -> int {
    let acc = 0;
    let i = 0;
    while i < n {
        acc = acc + i % 9;
        i = i + 2;
    }
    return acc;
}

// A product: each factor is one, so no chunk overflows.
fn ones(int n) -> int {
    let acc = 1;
    for x in 0..n {
        acc = acc * (x % 1 + 1);
    }
    return acc;
}

fn span_seq(int lo, int hi) -> int {
    let acc = 0;
    let x = lo;
    while x < hi {
        acc = acc + x % 7;
        x = x + 1;
    }
    return acc;
}

test("dynamic begin and end") {
    assert!(span(1000, 71000) == span_seq(1000, 71000));
    assert!(span(5, 9) == span_seq(5, 9));
    assert!(span(9, 5) == 0);
}

test("inclusive end") {
    // 0..=99999: 20000 cycles of 0..4.
    assert!(span_incl(0, 99999) == 20000 * 10);
    assert!(span_incl(3, 3) == 3);
}

test("stride two") {
    // 0, 2, .., 89998: 45000 terms, i % 9 over a full cycle of evens sums to 36.
    assert!(evens(90000) == 5000 * 36);
    assert!(evens(7) == 0 + 2 + 4 + 6);
}

test("product reduction") {
    assert!(ones(50000) == 1);
    assert!(ones(0) == 1);
}
