// #1331: an array literal of coroutine handles keeps them in the array
// object, as a `Vec` built with `push` does.
use tests::assert;

gen fn counter() {
    yield 0;
    yield 1;
}

gen fn from(int start) {
    yield start;
    yield start + 1;
}

test("a literal of handles, resumed by index") {
    let hs = [counter(), counter()];
    let a = resume hs[0];
    assert!(a == 0);
    let b = resume hs[0];
    assert!(b == 1);
    let c = resume hs[1];
    assert!(c == 0);
}

test("handles with arguments, resumed in a loop") {
    let hs = [from(10), from(20), from(30)];
    let total = 0;
    for i in 0..3 {
        total += resume hs[i];
    }
    assert!(total == 60);
}
