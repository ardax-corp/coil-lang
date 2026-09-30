// Provider for tests/positive/qualified_paths.hy: items reached by
// module-qualified path (`qpath_provider::Item`) without a `use` import.

// Pulls `qpath_provider::inner` into the compile; the test reaches the rest
// of that module by qualified path only.
use qpath_provider::inner::triple;

fn marker() -> int {
    return 7;
}

fn double(int x) -> int {
    return x * 2;
}

class Point {
    pub x: int,
    pub y: int,
}

impl Point {
    pub static fn origin() -> Point {
        return new Point(0, 0);
    }

    pub fn sum() -> int {
        return self.x + self.y;
    }
}

class Box<T> {
    pub value: T,
}

impl Box<T> {
    pub fn get() -> T {
        return self.value;
    }
}

enum Shape {
    Circle(int),
    Square(int),
    Empty,
}

impl Shape {
    pub fn area() -> int {
        return match self {
            Shape::Circle(r) => 3 * r * r,
            Shape::Square(s) => s * s,
            Shape::Empty => 0,
        };
    }
}

type Meters = int;

fn nine() -> int {
    return triple(3);
}

fn pick<T>(T a, T b) -> T {
    return b;
}
