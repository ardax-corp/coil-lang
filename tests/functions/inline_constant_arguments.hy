// A callee too big to inline is inlined at a call whose constant arguments
// fold it down: the copy runs the branch those constants pick, and the
// arguments that are not constant still flow in.
use tests::assert;

fn slow(int x) -> int {
    let i = 0;
    let s = 0;
    while i < x {
        s = s + i;
        i = i + 1;
    }
    return s;
}

fn pick(bool fast, int x) -> int {
    if fast {
        return x + 1;
    }
    return slow(slow(slow(slow(slow(x)))));
}

fn steps(int mode, int x) -> int {
    if mode == 0 {
        return x;
    }
    if mode == 1 {
        return x * 2;
    }
    let a = slow(x);
    let b = slow(a);
    let c = slow(b);
    return slow(c) + slow(x);
}

fn early(int n, int x) -> int {
    let t = 0;
    while t < 10 {
        if n > 3 {
            return x;
        }
        t = t + 1;
    }
    return slow(slow(slow(slow(x)))) + slow(x);
}

test("the branch a constant argument picks") {
    let k = 5;
    assert!(pick(true, k) == 6);
    assert!(pick(false, 3) == 3);
}

test("guard returns on a constant mode") {
    let k = 7;
    assert!(steps(0, k) == 7);
    assert!(steps(1, k) == 14);
    assert!(steps(2, 3) == 6);
}

test("a constant that does not fold the loop keeps the call") {
    let k = 9;
    assert!(early(5, k) == 9);
    assert!(early(1, 2) == 1);
}
