// A lambda literal called right where it is written (#1799): a block
// body, arguments, a capture, a unit result, and a returned closure.
use tests::assert;

test("call a block-bodied lambda in place") {
    let y = (fn () {
        let a = 2;
        return a * 21;
    })();
    assert!(y == 42);
}

test("call a lambda with arguments in place") {
    assert!((fn (int a, int b) => a * 10 + b)(4, 2) == 42);
    assert!((fn (string s) => s + "!")("hi") == "hi!");
}

test("call a capturing lambda in place") {
    let k = 40;
    assert!((fn (int x) use (k) => x + k)(2) == 42);
}

test("call a unit lambda in place") {
    let v = Vec::new();
    (fn () use (mut v) {
        v.push(42);
    })();
    assert!(v.len() == 1);
}

test("call a lambda's returned closure in place") {
    assert!((fn (int a) => fn (int b) use (a) => a + b)(40)(2) == 42);
}

test("call a lambda whose error type nothing pins in place") {
    let r = (fn () => Ok(2))();
    let n = match r {
        Ok(v) => v,
        Err(_) => 0,
    };
    assert!(n == 2);
}
