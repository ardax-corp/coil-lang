// A closed enum with an `impl` (but no drop hook) returns in two slots like
// any other unary enum. Methods, matches, generic code, containers, Option
// and closures all see the same values.
use tests::assert;

enum Tagged {
    Num(int),
    Empty,
}

impl Tagged {
    pub fn num() -> int {
        return match self {
            Tagged::Num(n) => n,
            Tagged::Empty => -1,
        };
    }

    pub fn bump(int by) -> Tagged {
        return match self {
            Tagged::Num(n) => Tagged::Num(n + by),
            Tagged::Empty => Tagged::Empty,
        };
    }
}

fn make(int n) -> Tagged {
    if n < 0 {
        return Tagged::Empty;
    }
    return Tagged::Num(n);
}

fn pick(bool first, Tagged a, Tagged b) -> Tagged {
    if first {
        return a;
    }
    return b;
}

fn ident<T>(T x) -> T {
    return x;
}

fn find(Vec<Tagged> xs, int want) -> Option<Tagged> {
    let i = 0;
    while i < xs.len() {
        if xs[i].num() == want {
            return Option::Some(xs[i]);
        }
        i = i + 1;
    }
    return Option::None;
}

test("impl enum returned from a call") {
    assert!(make(4).num() == 4);
    assert!(make(-1).num() == -1);
    let t = make(9);
    let n = match t {
        Tagged::Num(x) => x,
        Tagged::Empty => 0,
    };
    assert!(n == 9 && t.num() == 9);
}

test("impl enum through methods returning it") {
    assert!(make(1).bump(2).bump(3).num() == 6);
    assert!(make(-5).bump(2).num() == -1);
}

test("impl enum as parameters and generic values") {
    assert!(pick(true, make(1), make(2)).num() == 1);
    assert!(pick(false, make(1), make(-1)).num() == -1);
    assert!(ident(make(7)).num() == 7);
}

test("impl enum in containers and Option") {
    let xs: Vec<Tagged> = [make(1), make(-1), make(3)];
    xs.push(make(5));
    assert!(xs.len() == 4 && xs[3].num() == 5 && xs[1].num() == -1);
    let hit = find(xs, 3);
    assert!(
        match hit {
            Option::Some(t) => t.num() == 3,
            Option::None => false,
        },
    );
    assert!(
        match find(xs, 42) {
            Option::Some(_) => false,
            Option::None => true,
        },
    );
}

test("impl enum captured by a closure") {
    let t = make(11);
    let f = fn (int k) use (t) => t.num() + k;
    assert!(f(1) == 12);
}
