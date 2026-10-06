// Ground trait calls whose arguments or result are user enums: the enum
// word passes boxed, as the instance entry takes it.
enum Shape {
    Dot,
    Circle(int),
}

trait Make<T> {
    static fn make(int n) -> T {}
}

impl Make for Shape {
    pub static fn make(int n) -> Shape {
        if n == 0 {
            return Shape::Dot;
        }
        return Shape::Circle(n);
    }
}

trait Area<T> {
    fn area(T s) -> int {}
}

impl Area for Shape {
    pub fn area(Shape s) -> int {
        return match s {
            Shape::Dot => 0,
            Shape::Circle(r) => 3 * r * r,
        };
    }
}

fn built(int n) -> Shape {
    return Shape::make(n);
}

fn measured(int n) -> int {
    return area(Shape::make(n));
}

test("static trait method returning an enum") {
    assert(area(built(0)) == 0)?;
    assert(area(built(2)) == 12)?;
}

test("function-style trait call on an enum") {
    assert(measured(1) == 3)?;
    assert(measured(0) == 0)?;
}
