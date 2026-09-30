// `static fn` in a trait: no `Self`-typed receiver, called as `Owner::m(..)`
// at a concrete owner (class, primitive, enum) or `T::m(..)` under a bound
// (#524). A return-only `T` is chosen by the expected type.

class Val {
    pub i: int,
    pub s: string,
}

class Point {
    pub x: int,
    pub name: string,
}

enum Dir {
    Up,
    Down,
}

enum DecodeError {
    Bad,
}

trait FromVal<T> {
    static fn from_val(Val v) -> T {}

    // A default static body reaches its sibling through the dictionary.
    static fn from_int(int i) -> T {
        return from_val(new Val(i, "d"));
    }
}

impl FromVal for Point {
    pub static fn from_val(Val v) -> Point {
        return new Point(v.i, v.s);
    }
}

impl FromVal for int {
    pub static fn from_val(Val v) -> int {
        return v.i * 2;
    }
}

impl FromVal for Dir {
    pub static fn from_val(Val v) -> Dir {
        if v.i == 0 {
            return Dir::Up;
        }
        return Dir::Down;
    }
}

trait TryFromVal<T> {
    static fn try_from_val(Val v) -> Result<T, DecodeError> {}
}

impl TryFromVal for Point {
    pub static fn try_from_val(Val v) -> Result<Point, DecodeError> {
        if v.i < 0 {
            return Result::Err(DecodeError::Bad);
        }
        return Result::Ok(new Point(v.i, v.s));
    }
}

// Return-only `T`: the shared body dispatches through the dictionary.
fn decode_as<T: FromVal>(Val v) -> T {
    return T::from_val(v);
}

// `T` from an argument: the ground call monomorphizes.
fn decode_like<T: FromVal>(T proto, Val v) -> T {
    let _ = proto;
    return T::from_val(v);
}

fn decode_default<T: FromVal>(int i) -> T {
    return T::from_int(i);
}

fn try_decode<T: TryFromVal>(Val v) -> Result<T, DecodeError> {
    let t = T::try_from_val(v)?;
    return Result::Ok(t);
}

fn x_of(Result<Point, DecodeError> r) -> int {
    let x = match r {
        Result::Ok(p) => p.x,
        Result::Err(_) => -1,
    };
    return x;
}

fn decode_twice(Val v) -> Result<int, DecodeError> {
    // `f()?` in a Result fn expects `Result<Point, DecodeError>` of the call.
    let p: Point = try_decode(v)?;
    let q: Point = Point::try_from_val(v)?;
    return Result::Ok(p.x + q.x);
}

fn is_up(Dir d) -> bool {
    let r = match d {
        Dir::Up => true,
        Dir::Down => false,
    };
    return r;
}

test("concrete owner: class, primitive, enum") {
    let p = Point::from_val(new Val(4, "a"));
    assert(p.x == 4)?;
    assert(p.name == "a")?;
    assert(int::from_val(new Val(21, "")) == 42)?;
    assert(is_up(Dir::from_val(new Val(0, ""))))?;
    assert(!is_up(Dir::from_val(new Val(1, ""))))?;
}

test("default static body reaches a sibling") {
    let p = Point::from_int(7);
    assert(p.x == 7)?;
    assert(p.name == "d")?;
    assert(int::from_int(5) == 10)?;
}

test("T::m under a bound, return-only T chosen by the annotation") {
    let p: Point = decode_as(new Val(9, "z"));
    assert(p.x == 9)?;
    let n: int = decode_as(new Val(3, ""));
    assert(n == 6)?;
}

test("T::m under a bound, T from an argument (mono clone)") {
    let p = decode_like(new Point(0, ""), new Val(11, "m"));
    assert(p.x == 11)?;
    assert(decode_like(1, new Val(4, "")) == 8)?;
}

test("T::m default body under a bound") {
    let p: Point = decode_default(12);
    assert(p.x == 12)?;
}

test("static method returning Result<T, E>") {
    assert(x_of(Point::try_from_val(new Val(2, "q"))) == 2)?;
    assert(x_of(Point::try_from_val(new Val(-2, "q"))) == -1)?;
    let via_t: Result<Point, DecodeError> = try_decode(new Val(5, "w"));
    assert(x_of(via_t) == 5)?;
    let bad: Result<Point, DecodeError> = try_decode(new Val(-5, "w"));
    assert(x_of(bad) == -1)?;
}

test("`?` on a return-only T inside a Result fn") {
    let ok = match decode_twice(new Val(3, "")) {
        Result::Ok(n) => n,
        Result::Err(_) => -1,
    };
    assert(ok == 6)?;
    let bad = match decode_twice(new Val(-3, "")) {
        Result::Ok(n) => n,
        Result::Err(_) => -1,
    };
    assert(bad == -1)?;
}
