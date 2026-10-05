// HIR lowering of `static let` reads and writes: module statics, class
// statics, an `Option` static, and a parameter that shadows a static.

static let hits: int = 0;

static let last: Option<int> = Option::None;

class Counter {
    pub static count: int = 0,
    pub value: int,
}

impl Counter {
    pub fn bump() {
        Counter::count = Counter::count + 1;
        self.value += 1;
    }
}

fn hit(int n) -> int {
    hits += n;
    last = Option::Some(n);
    return hits;
}

fn last_or(int d) -> int {
    return match last {
        Option::Some(v) => v,
        Option::None => d,
    };
}

fn shadowed(int hits) -> int {
    return hits * 2;
}

test("module statics") {
    assert(last_or(-1) == -1)?;
    assert(hit(2) == 2)?;
    assert(hit(3) == 5)?;
    assert(last_or(-1) == 3)?;
    assert(shadowed(21) == 42)?;
    assert(hits == 5)?;
}

test("class statics") {
    let c = new Counter(0);
    let before = Counter::count;
    c.bump();
    c.bump();
    assert(Counter::count == before + 2)?;
    assert(c.value == 2)?;
}
