// Inherent methods may be called before their `impl` appears in the file
// (no later free fn forces the phased emit order): instance and static
// calls, in operands that stage their siblings.

class Point {
    pub x: int,
}

fn combined() -> int {
    return new Point(3).double() + Point::origin().x;
}

fn instance_only() -> int {
    return new Point(3).double();
}

impl Point {
    pub fn double() -> int {
        return self.x * 2;
    }

    pub static fn origin() -> Point {
        return new Point(10);
    }
}

test("instance method declared after its caller") {
    assert(instance_only() == 6)?;
}

test("instance and static calls in one operand pair") {
    assert(combined() == 16)?;
}
