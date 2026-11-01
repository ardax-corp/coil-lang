// Nested tuple `let` patterns read their items out of the inner heap
// tuples on direct MIR, at every opt level (at -O 0 no tuple is split).
use tests::assert;

fn deep(int v) -> int {
    let (a, (b, (c, d))) = (v, (v + 1, (v + 2, v + 3)));
    return a * 1000 + b * 100 + c * 10 + d;
}

fn skip(int v) -> int {
    let ((a, _, c), d) = ((v, v * 2, v * 3), v * 4);
    return a + c + d;
}

fn three(int v) -> int {
    let (x, (y, z), w) = (v, (v + 1, v + 2), v + 3);
    return x + y * 10 + z * 100 + w * 1000;
}

test("nested tuple let patterns") {
    assert!(deep(1) == 1234);
    assert!(skip(2) == 16);
    assert!(three(1) == 4321);
}
