// Ground trait instance methods take their arguments raw: operator,
// method-style and function-style calls pass the instance's own words, so
// the method lowers to direct MIR. Calls through a dictionary (a shared
// generic body, a default body) box the arguments and reach the method
// through its `$dict` adapter, which unboxes them first.

trait Weigh<T> {
    fn weight(T x) -> int {}
    fn heavier(T a, T b) -> T {}
    fn keep(T x, Option<T> o) -> T {}

    fn double(T x) -> int {
        return x.weight() * 2;
    }
}

// A generic caller emitted before the instances: its dictionary already
// names the adapters.
fn weigh_all<T: Weigh>(T a, T b) -> int {
    return a.weight() + b.weight() + a.heavier(b).weight();
}

impl Weigh for int {
    pub fn weight(int x) -> int {
        return x * 3;
    }

    pub fn heavier(int a, int b) -> int {
        if a >= b {
            return a;
        }
        return b;
    }

    pub fn keep(int x, Option<int> o) -> int {
        return match o {
            Option::Some(v) => v + x,
            Option::None => x,
        };
    }
}

class Crate {
    pub kg: int,
}

impl Weigh for Crate {
    pub fn weight(Crate c) -> int {
        return c.kg + 1;
    }

    pub fn heavier(Crate a, Crate b) -> Crate {
        if a.kg >= b.kg {
            return a;
        }
        return b;
    }

    pub fn keep(Crate x, Option<Crate> o) -> Crate {
        return match o {
            Option::Some(c) => c,
            Option::None => x,
        };
    }
}

class V2 {
    pub x: int,
    pub y: int,
}

impl Add for V2 {
    pub fn add(V2 a, V2 b) -> V2 {
        return new V2(a.x + b.x, a.y + b.y);
    }
}

impl Neg for V2 {
    pub fn neg(V2 a) -> V2 {
        return new V2(0 - a.x, 0 - a.y);
    }
}

#[derive(Eq, Ord)]
enum Rank {
    Low,
    Mid,
    High,
}

fn walk(int n) -> V2 {
    let acc = new V2(0, 0);
    for i in 0..n {
        acc = acc + new V2(i, 1);
    }
    return -acc;
}

fn top(Rank a, Rank b) -> int {
    let n = 0;
    if a < b {
        n += 1;
    }
    if a == b {
        n += 10;
    }
    return n;
}

fn direct(int k) -> int {
    let c = new Crate(k);
    return k.weight() + c.weight() + weight(k) + c.heavier(new Crate(1)).kg;
}

fn kept(int k) -> int {
    return k.keep(Option::Some(1)) + k.keep(Option::None) + new Crate(k).keep(Option::None).kg;
}

test("operators on raw instances") {
    let v = walk(4);
    assert(v.x == -6)?;
    assert(v.y == -4)?;
    assert(top(Rank::Low, Rank::High) == 1)?;
    assert(top(Rank::Mid, Rank::Mid) == 10)?;
    assert(top(Rank::High, Rank::Low) == 0)?;
}

test("method and function-style calls pass raw words") {
    assert(direct(5) == 15 + 6 + 15 + 5)?;
    assert(kept(4) == 5 + 4 + 4)?;
}

test("dictionary calls go through the adapter") {
    assert(weigh_all(2, 7) == 6 + 21 + 21)?;
    assert(weigh_all(new Crate(3), new Crate(1)) == 4 + 2 + 4)?;
    assert(7.double() == 42)?;
    assert(new Crate(2).double() == 6)?;
}
