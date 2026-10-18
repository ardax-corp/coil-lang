// An impl method calls a generic method declared after it in the same impl:
// the forward stub's signature reads the later method's own `<V>` as a type
// variable, not as a concrete type `V` (#1461).
use tests::assert;

class Cell<T> {
    pub v: T,
}

impl Cell<T> {
    pub fn echo<U>(U x) -> U {
        return self.same(x);
    }

    pub fn same<V>(V y) -> V {
        return y;
    }

    pub static fn s_echo<U>(U x) -> U {
        return Cell::s_same(x);
    }

    pub static fn s_same<V>(V y) -> V {
        return y;
    }

    pub fn shown<U: Show>(U x) -> string {
        return self.show_it(x);
    }

    pub fn show_it<V: Show>(V y) -> string {
        return "{y}";
    }
}

class Plain {
    pub n: int,
}

impl Plain {
    pub fn echo<U>(U x) -> U {
        return self.same(x);
    }

    pub fn same<V>(V y) -> V {
        return y;
    }
}

test("generic impl method calls a later generic method") {
    let c = new Cell(1);
    assert!(c.echo("hi") == "hi");
    assert!(c.echo(3) == 3);
    assert!(Cell::s_echo(4) == 4);
    assert!(Cell::s_echo("s") == "s");
    assert!(c.shown(7) == "7");
    assert!(c.shown("w") == "w");
}

test("non-generic impl method calls a later generic method") {
    let p = new Plain(1);
    assert!(p.echo("x") == "x");
    assert!(p.echo(5) == 5);
}
