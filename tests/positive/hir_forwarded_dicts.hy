// A generic body passes its own trait dictionaries on to a generic callee's
// shared body, ahead of any the call site resolves itself.
use string::format;

trait Weigh<T> {
    fn weight(T x) -> int {}
}

trait Label<T> {
    fn label(T x) -> string {}
}

class Crate {
    pub kg: int,
}

impl Weigh for int {
    pub fn weight(int x) -> int {
        return x * 2;
    }
}

impl Weigh for Crate {
    pub fn weight(Crate c) -> int {
        return c.kg * 10;
    }
}

impl Label for int {
    pub fn label(int x) -> string {
        return "int";
    }
}

impl Label for Crate {
    pub fn label(Crate c) -> string {
        return "crate";
    }
}

fn heavy<T: Weigh>(T x) -> int {
    return x.weight() + 1;
}

fn twice<T: Weigh>(T x) -> int {
    return heavy(x) + heavy(x);
}

fn tagged<T: Weigh, U: Label>(T x, U u) -> string {
    return u.label() + ":" + format("%i", heavy(x));
}

fn relay<T: Weigh, U: Label>(T x, U u) -> string {
    return tagged(x, u);
}

class Scale {
    pub bias: int,
}

impl Scale {
    pub fn read<T: Weigh>(T x) -> int {
        return x.weight() + self.bias;
    }
}

fn via_scale<T: Weigh>(Scale s, T x) -> int {
    return s.read(x);
}

test("a generic fn forwards to a generic fn") {
    assert(twice(3) == 14)?;
    assert(twice(new Crate(2)) == 42)?;
}

test("two dictionaries forwarded together") {
    assert(relay(4, new Crate(1)) == "crate:9")?;
    assert(relay(new Crate(1), 4) == "int:11")?;
}

test("a generic fn forwards to a generic method") {
    assert(via_scale(new Scale(5), 3) == 11)?;
    assert(via_scale(new Scale(5), new Crate(3)) == 35)?;
}
