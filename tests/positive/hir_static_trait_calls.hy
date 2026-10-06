// Static trait methods: `Owner::m(..)` at a concrete owner calls the
// instance directly; `T::m(..)` under a bound goes through the dictionary
// or, in a mono clone, the concrete instance.
trait Make<S> {
    static fn make(int n) -> S {}
    static fn tag() -> int {}
}

class Coin {
    pub v: int,
}

impl Make for Coin {
    pub static fn make(int n) -> Coin {
        return new Coin(n * 2);
    }

    pub static fn tag() -> int {
        return 7;
    }
}

impl Make for int {
    pub static fn make(int n) -> int {
        return n + 1;
    }

    pub static fn tag() -> int {
        return 3;
    }
}

fn tags<T: Make>(T x) -> int {
    return T::tag() * 10;
}

test("static trait methods") {
    assert(Coin::make(4).v == 8)?;
    assert(Coin::tag() + int::tag() == 10)?;
    assert(int::make(5) == 6)?;
    assert(tags(new Coin(1)) == 70)?;
    assert(tags(2) == 30)?;
}
