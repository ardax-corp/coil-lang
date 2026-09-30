// Method-level type parameters with trait bounds on inherent `impl`
// methods: the bound is in scope in the body and dispatched from the call
// site, for instance, static, multi-parameter, and `where` forms (#523).

class Val {
    pub i: int,
}

class Point {
    pub x: int,
}

trait FromVal<T> {
    fn hydrate_plain(T proto, Val v) -> T {}
}

impl FromVal for Point {
    pub fn hydrate_plain(Point proto, Val v) -> Point {
        return new Point(v.i);
    }
}

impl FromVal for int {
    pub fn hydrate_plain(int proto, Val v) -> int {
        return proto + v.i;
    }
}

trait Tag<T> {
    fn tag(T x) -> int {}
}

impl Tag for Point {
    pub fn tag(Point x) -> int {
        return 100;
    }
}

class Codec {
    m: int,
}

impl Codec {
    pub fn decode<T: FromVal>(T proto, Val v) -> T {
        return proto.hydrate_plain(v);
    }

    pub fn decode_plus<T: FromVal>(T proto, Val v) -> T {
        return proto.hydrate_plain(new Val(v.i + self.m));
    }

    pub static fn make<T: FromVal>(T proto, int k) -> T {
        return proto.hydrate_plain(new Val(k));
    }

    pub fn both<T: FromVal, U: Tag>(T proto, U u, Val v) -> int {
        return u.tag() + v.i;
    }

    pub fn tagged<T>(T x) -> int where Tag<T> {
        return x.tag();
    }
}

fn via_generic<T: FromVal>(Codec c, T proto, int k) -> T {
    return c.decode(proto, new Val(k));
}

test("method-level bound on an inherent method") {
    let c = new Codec(0);
    assert(c.decode(new Point(0), new Val(5)).x == 5)?;
}

test("method-level bound with a primitive instance") {
    assert(new Codec(0).decode(10, new Val(5)) == 15)?;
}

test("method body uses self alongside the bound") {
    assert(new Codec(3).decode_plus(new Point(0), new Val(5)).x == 8)?;
}

test("static method with a bound") {
    assert(Codec::make(new Point(0), 7).x == 7)?;
}

test("two method type params with bounds") {
    assert(new Codec(0).both(new Point(0), new Point(0), new Val(4)) == 104)?;
}

test("where clause on a method") {
    assert(new Codec(0).tagged(new Point(1)) == 100)?;
}

test("generic method called from a generic fn") {
    assert(via_generic(new Codec(0), new Point(0), 6).x == 6)?;
    assert(via_generic(new Codec(0), 1, 6) == 7)?;
}
