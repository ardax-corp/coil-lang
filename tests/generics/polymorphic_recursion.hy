// Generic code that calls itself at a growing type compiles in bounded
// time: the clone that would repeat a function (or method) already on
// its own chain of clones takes the shared body instead.
use tests::assert;

fn f<T>(T x, int n) -> int {
    if n == 0 {
        return 0;
    }
    return 1 + g((x, 1), n - 1);
}

fn g<T>(T x, int n) -> int {
    if n == 0 {
        return 0;
    }
    return 1 + f((x, 2), n - 1);
}

class Nest<T> {
    pub item: T,
}

impl Nest<T> {
    pub fn depth(int n) -> int {
        if n == 0 {
            return 0;
        }
        let inner = new Nest(self);
        return 1 + inner.depth(n - 1);
    }

    pub fn down(int n) -> int {
        return go(self, n);
    }
}

fn go<T>(T x, int n) -> int {
    if n == 0 {
        return 0;
    }
    return 1 + new Nest(x).down(n - 1);
}

test("mutual free functions") {
    assert!(f(1, 20) == 20);
    assert!(f("s", 3) == 3);
}

test("method on a growing receiver") {
    assert!(new Nest(1).depth(20) == 20);
}

test("free function and method") {
    assert!(go(1, 20) == 20);
    assert!(go(1.5, 0) == 0);
}
