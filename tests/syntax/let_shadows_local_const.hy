// A `let`, loop binding or parameter named like a local `const` is a new,
// mutable binding (coil-lang#1146); the const itself stays constant.
use tests::assert;

test("inner block let shadows a local const") {
    const x = 1;
    {
        let x = 2;
        x = 3;
        assert!(x == 3);
    }
    assert!(x == 1);
}

test("same-scope let after a local const") {
    const x = 1;
    let x = 2;
    x += 5;
    assert!(x == 7);
}

test("for binding reuses a local const name") {
    const i = 100;
    let total = 0;
    for i in 0..3 {
        i = i * 2;
        total += i;
    }
    assert!(total == 6);
    assert!(i == 100);
}

test("lambda parameter reuses a local const name") {
    const n = 4;
    let f = fn (int n) {
        n = n + 1;
        return n;
    };
    assert!(f(1) == 2);
    assert!(n == 4);
}
