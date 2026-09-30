// A ground trait method call whose receiver / arguments are `new C(..)`
// (built in temp slots) leaves its result where the enclosing expression
// expects it: `if let`, match, operands and call arguments.

class Val {
    pub i: int,
}

class Point {
    pub x: int,
}

enum DecErr {
    Bad,
}

trait FromVal<T> {
    fn hydrate(T proto, Val v) -> Result<T, DecErr> {}
}

trait Size<T> {
    fn size(T p, Val v) -> int {}
}

impl FromVal for Point {
    pub fn hydrate(Point proto, Val v) -> Result<Point, DecErr> {
        if v.i < 0 {
            return Result::Err(DecErr::Bad);
        }
        return Result::Ok(new Point(v.i));
    }
}

impl Size for Point {
    pub fn size(Point p, Val v) -> int {
        return p.x + v.i;
    }
}

fn plus(int a, int b) -> int {
    return a + b;
}

fn if_let(int i) -> int {
    if let Result::Ok(p) = new Point(0).hydrate(new Val(i)) {
        return 1000 + p.x;
    }
    return -1;
}

fn in_match(int i) -> int {
    let r = match new Point(0).hydrate(new Val(i)) {
        Result::Ok(p) => p.x,
        Result::Err(_) => -1,
    };
    return r;
}

test("if let on a ground trait call with new operands") {
    assert(if_let(7) == 1007)?;
    assert(if_let(-7) == -1)?;
}

test("match on a ground trait call with new operands") {
    assert(in_match(7) == 7)?;
    assert(in_match(-7) == -1)?;
}

test("ground trait call as an operand and an argument") {
    assert(new Point(2).size(new Val(3)) + 10 == 15)?;
    assert(plus(1, new Point(2).size(new Val(3))) == 6)?;
    assert(plus(new Point(2).size(new Val(3)), new Point(4).size(new Val(5))) == 14)?;
}
