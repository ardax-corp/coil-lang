// Trait impl methods returning `Result` get the implicit `Ok` wrap in the
// return's real layout (here a pointer niche), per instance, without leaking
// result mode onto a free fn of the same name (#520).

class Val {
    pub i: int,
}

class Point {
    pub x: int,
}

class Line {
    pub a: int,
    pub b: int,
}

enum DecErr {
    Bad,
}

trait FromVal<T> {
    fn hydrate(T proto, Val v) -> Result<T, DecErr> {}
}

impl FromVal for Point {
    pub fn hydrate(Point proto, Val v) -> Result<Point, DecErr> {
        if v.i < 0 {
            return Result::Err(DecErr::Bad);
        }
        return new Point(v.i);
    }
}

impl FromVal for Line {
    pub fn hydrate(Line proto, Val v) -> Result<Line, DecErr> {
        let l = new Line(v.i, v.i * 2);
        return l;
    }
}

trait Mk<T> {
    fn mk(T proto, int n) -> Result<T, string> {}
}

impl Mk for Point {
    pub fn mk(Point proto, int n) -> Result<Point, string> {
        return new Point(n);
    }
}

// Shares the trait method's bare name; not a Result function.
fn mk(int n) -> int {
    return n * 10;
}

fn point_x(Val v) -> int {
    let r = match new Point(0).hydrate(v) {
        Result::Ok(p) => p.x,
        Result::Err(_) => -1,
    };
    return r;
}

fn line_b(Val v) -> int {
    let r = match new Line(0, 0).hydrate(v) {
        Result::Ok(l) => l.b,
        Result::Err(_) => -1,
    };
    return r;
}

fn mk_x(int n) -> int {
    let r = match new Point(0).mk(n) {
        Result::Ok(p) => p.x,
        Result::Err(_) => -1,
    };
    return r;
}

test("implicit Ok from a trait impl") {
    assert(point_x(new Val(9)) == 9)?;
}

test("implicit Ok of a local from a second instance") {
    assert(line_b(new Val(4)) == 8)?;
}

test("explicit Err from a trait impl") {
    assert(point_x(new Val(-3)) == -1)?;
}

test("free fn sharing a trait method name keeps its own return") {
    assert(mk(3) == 30)?;
    assert(mk_x(5) == 5)?;
}

trait Parse<T> {
    fn parse(T proto, int n) -> Result<T, string> {}

    fn parse_or(T proto, int n, int d) -> int {
        return match proto.parse(n) {
            Result::Ok(_) => n,
            Result::Err(_) => d,
        };
    }
}

// `Result<int, string>` is a two-word return shape; an instance method stays
// one word so its dictionary entry can take its address.
impl Parse for int {
    pub fn parse(int proto, int n) -> Result<int, string> {
        if n < 0 {
            return Result::Err("neg");
        }
        return n * 2;
    }
}

fn parse_ground(int n) -> int {
    let r = match 0.parse(n) {
        Result::Ok(v) => v,
        Result::Err(_) => -1,
    };
    return r;
}

test("int instance: implicit Ok on a two-word Result shape") {
    assert(parse_ground(4) == 8)?;
    assert(parse_ground(-4) == -1)?;
}

test("int instance: default body through the dictionary") {
    assert(0.parse_or(4, 9) == 4)?;
    assert(0.parse_or(-4, 9) == 9)?;
}
