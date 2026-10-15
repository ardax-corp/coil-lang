// Calls to a shared generic body that takes bare type parameters: each
// such argument is boxed to its tag and the result unboxed, from a
// caller lowered on direct MIR.
use tests::assert;

fn cycle<T>(int n, T a, T b, T c) -> T {
    let x = a;
    let y = b;
    let z = c;
    let i = 0;
    while i < n {
        let t = x;
        x = y;
        y = z;
        z = t;
        i = i + 1;
    }
    return x;
}

test("ints through a shared generic") {
    assert!(cycle(0, 1, 2, 3) == 1);
    assert!(cycle(2, 1, 2, 3) == 3);
    assert!(cycle(1, 4, 5, 6) + 1 == 6);
}

test("floats and bools through a shared generic") {
    assert!(cycle(1, 1.5, 2.5, 3.5) == 2.5);
    assert!(cycle(3, true, false, false));
    assert!(!cycle(1, true, false, true));
}

test("results used in a loop") {
    let total = 0;
    for i in 0..6 {
        total = total + cycle(i, 1, 10, 100);
    }
    assert!(total == 222);
}
