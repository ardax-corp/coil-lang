// A payload variant of an enum with `fn drop()` made in a body lowered to
// direct MIR is tagged for its finalizer: built in a loop, with a heap
// payload, with two payload words, and kept alive past a collection.
use tests::assert;
use gc::collect;

static let drops: int = 0;

static let total: int = 0;

enum Res {
    Pair(int, int),
    Named(string),
    Gone,
}

impl Res {
    fn drop() {
        drops = drops + 1;
        total = total + match self {
            Res::Pair(a, b) => a + b,
            Res::Named(s) => s.len(),
            Res::Gone => 0,
        };
    }
}

fn pair_sum(int a, int b) -> int {
    let r = Res::Pair(a, b);
    return match r {
        Res::Pair(x, y) => x + y,
        default => 0,
    };
}

fn churn(int n) -> int {
    let acc = 0;
    let i = 0;
    while i < n {
        let r = Res::Pair(i, 1);
        acc = acc + match r {
            Res::Pair(x, y) => x * y,
            default => 0,
        };
        i = i + 1;
    }
    return acc;
}

fn name_len(string s) -> int {
    let r = Res::Named(s);
    return match r {
        Res::Named(t) => t.len(),
        default => 0,
    };
}

fn unit_only() -> int {
    let r = Res::Gone;
    return match r {
        Res::Gone => 1,
        default => 0,
    };
}

test("two payload words drop once") {
    drops = 0;
    total = 0;
    assert!(pair_sum(3, 4) == 7);
    collect();
    assert!(drops == 1);
    assert!(total == 7);
}

test("each value made in a loop drops") {
    drops = 0;
    total = 0;
    assert!(churn(20) == 190);
    collect();
    assert!(drops == 20);
    assert!(total == 190 + 20);
}

test("heap payload drops") {
    drops = 0;
    total = 0;
    assert!(name_len("abcde") == 5);
    collect();
    assert!(drops == 1);
    assert!(total == 5);
}

test("unit variant never drops") {
    drops = 0;
    assert!(unit_only() == 1);
    collect();
    assert!(drops == 0);
}
