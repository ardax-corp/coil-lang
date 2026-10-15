// Static initializers may use arithmetic, branches, matches, allocations and
// calls; the values persist, and heap statics survive collection.
use tests::assert;
use gc::collect;

class Pair {
    pub a: int,
    pub b: int,
}

enum Shape {
    Dot,
    Square(int),
}

fn square(int n) -> int {
    return n * n;
}

fn sum_to(int n) -> int {
    let s = 0;
    let i = 1;
    while i <= n {
        s = s + i;
        i = i + 1;
    }
    return s;
}

static const LIMIT: int = 12;

static let scaled: int = LIMIT * 3 + square(4) - 1;

static let picked: int = sum_to(scaled / 5) - sum_to(scaled / 10 - 5) + (scaled > 40) as int;

static let pair: Pair = new Pair(square(3), sum_to(LIMIT));

static let shape: Shape = Shape::Square(picked);

static let area: int = match shape {
    Shape::Dot => 0,
    Shape::Square(s) => s * s,
};

static let primes: Vec<int> = [2, 3, 5, 7, 11];

static let greeting: string = "static " + "init";

static let maybe: Option<Pair> = Option::Some(new Pair(1, 2));

test("static initializers lowered to direct MIR") {
    let junk = 0;
    while junk < 1000 {
        let t = new Pair(junk, junk);
        junk = junk + t.a - t.b + 1;
    }
    collect();
    assert!(scaled == 51);
    assert!(picked == 56);
    assert!(pair.a == 9);
    assert!(pair.b == 78);
    assert!(area == 3136);
    assert!(len(primes) == 5);
    assert!(primes[4] == 11);
    assert!(greeting == "static init");
    let b = match maybe {
        Option::Some(p) => p.b,
        Option::None => -1,
    };
    assert!(b == 2);
}
