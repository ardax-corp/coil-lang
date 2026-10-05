// A generic class's shared method bodies lower from HIR when they return or
// hold its type parameter: `T`, `Option<T>`, `Vec<T>`, `(T, int)` and the
// class itself are boxed words, as the AST codegen lays them out.

class Holder<T> {
    item: T,
    spare: Option<T>,
}

impl Holder<T> {
    pub static fn new(T item) -> Holder<T> {
        return new Holder(item, Option::None);
    }

    pub fn get() -> T {
        return self.item;
    }

    pub fn take_spare() -> Option<T> {
        let s = self.spare;
        self.spare = Option::None;
        return s;
    }

    pub fn put_spare(T v) {
        self.spare = Option::Some(v);
    }

    pub fn copy() -> Holder<T> {
        let b = Holder::new(self.item);
        return b;
    }

    pub fn both() -> Vec<T> {
        let out: Vec<T> = Vec::new();
        out.push(self.item);
        out.push(self.item);
        return out;
    }

    pub fn tagged(int n) -> (T, int) {
        return (self.item, n);
    }

    pub fn next_tagged(int n) -> Option<(T, int)> {
        if n < 0 {
            return Option::None;
        }
        return Option::Some((self.item, n));
    }

    pub fn spare_or(T fallback) -> T {
        return match self.spare {
            Option::Some(v) => v,
            Option::None => fallback,
        };
    }
}

test("int holder") {
    let b = Holder::new(5);
    assert(b.get() == 5)?;
    assert(
        match b.take_spare() {
            Option::Some(_) => false,
            Option::None => true,
        },
    )?;
    b.put_spare(7);
    assert(b.spare_or(1) == 7)?;
    assert(
        match b.take_spare() {
            Option::Some(v) => v,
            Option::None => 0,
        } == 7,
    )?;
    assert(b.spare_or(1) == 1)?;
    assert(b.copy().get() == 5)?;
    let v = b.both();
    assert(v.len() == 2 && v[1] == 5)?;
    let t = b.tagged(3);
    assert(t[0] == 5 && t[1] == 3)?;
    assert(
        match b.next_tagged(4) {
            Option::Some(p) => p[0] + p[1],
            Option::None => 0,
        } == 9,
    )?;
    assert(
        match b.next_tagged(-1) {
            Option::Some(p) => p[1],
            Option::None => -2,
        } == -2,
    )?;
}

test("string holder") {
    let b = Holder::new("a");
    b.put_spare("z");
    assert(b.copy().get() == "a")?;
    assert(b.both()[0] == "a")?;
    assert(b.tagged(2)[0] == "a")?;
    assert(
        match b.next_tagged(1) {
            Option::Some(p) => p[0],
            Option::None => "",
        } == "a",
    )?;
    assert(b.spare_or("y") == "z")?;
    assert(
        match b.take_spare() {
            Option::Some(v) => v,
            Option::None => "",
        } == "z",
    )?;
    assert(b.spare_or("y") == "y")?;
}

// A field open in the parameter (`Option<Link<T>>`) is a boxed enum in the
// shared body, not the niche a closed `Option<Link<int>>` would be.
class Link<T> {
    pub value: T,
    pub next: Option<Link<T>>,
}

class Chain<T> {
    head: Option<Link<T>>,
    pub len: int,
}

impl Chain<T> {
    pub static fn new() -> Chain<T> {
        return new Chain(Option::None, 0);
    }

    pub fn push(T v) {
        self.head = Option::Some(new Link(v, self.head));
        self.len = self.len + 1;
    }

    pub fn peek() -> Option<T> {
        return match self.head {
            Option::None => Option::None,
            Option::Some(n) => Option::Some(n.value),
        };
    }
}

test("open field layouts") {
    let c = Chain::new();
    assert((c.peek() ?? -7) == -7)?;
    c.push(4);
    c.push(9);
    assert((c.peek() ?? 0) == 9)?;
    assert(c.len == 2)?;
}
