// Heap values held in a frame survive a `collect` in that frame or below:
// array params and locals, a class local, `Vec` literals built from calls,
// a growing `Vec` (sieve), and a `Root` keeping a weak's referent alive.
use tests::assert;
use gc::{collect, get, root, weak, upgrade};

class B {
    pub v: int,
}

fn keep([int; 3] xs) -> [int; 3] {
    let junk = [1, 2, 3];
    collect();
    let r = xs;
    if len(xs) > 5 {
        r = junk;
    }
    return r;
}

fn keep_eq([int; 3] xs) -> [int; 3] {
    let junk = [1, 2, 3];
    collect();
    if junk == xs {
        return junk;
    }
    return xs;
}

fn fib(int n) -> int {
    if n < 2 {
        return n;
    }
    return fib(n - 1) + fib(n - 2);
}

fn holds(int n) -> int {
    let b = new B(n);
    collect();
    return b.v;
}

fn pair(int a, int b) -> Vec<int> {
    return [a, b];
}

fn leaf(int n) -> int {
    return n + 1;
}

fn wrap(int n) -> Vec<int> {
    return [leaf(n), n];
}

fn nsieve(int n) -> int {
    let flags: Vec<int> = Vec::with_capacity(n);
    let i = 0;
    while i < n {
        flags.push(1);
        i = i + 1;
    }
    let count = 0;
    let p = 2;
    while p < n {
        if flags[p] == 1 {
            count = count + 1;
            let k = p + p;
            while k < n {
                flags[k] = 0;
                k = k + p;
            }
        }
        p = p + 1;
    }
    return count;
}

test("an array parameter survives a collect in its callee") {
    let a = [42, 7, 0];
    let b = keep(a);
    assert!(b[0] == 42);
    let c = keep_eq(a);
    assert!(c[0] == 42);
}

test("a class local survives a collect") {
    assert!(fib(10) + holds(3) == 58);
}

test("vec literals from params and calls") {
    let xs = pair(3, 4);
    assert!(xs[0] + xs[1] == 7);
    let ys = wrap(2);
    assert!(ys[0] + ys[1] == 5);
}

test("a growing vec in a sieve") {
    assert!(nsieve(1 << 14) == 1900);
}

test("a root on the frame keeps a weak upgradable across collect") {
    let r = root([9, 8, 7]);
    let inner = match get(r) {
        Option::Some(v) => v,
        Option::None => {
            return;
        },
    };
    let w = weak(inner);
    let _freed = collect();
    let out = match upgrade(w) {
        Option::Some(_) => "some",
        Option::None => "none",
    };
    assert!(out == "some");
}
