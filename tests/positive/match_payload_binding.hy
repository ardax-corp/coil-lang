// Match payloads land where the scrutinee sat, whatever the arm reads first.
// Inlined helpers let slot promotion drop the binding copies, so an arm may
// start with a constant or with its second field.
enum Shape {
    Circle(int),
    Rect(int, int),
}

fn id(int x) -> int {
    return x;
}

/// Built in a dense body: payload order must survive `DenseMake`.
fn pick(int i) -> Shape {
    if i > 2 {
        return Shape::Rect(i, i * 10 + 1);
    }
    return Shape::Circle(i * 7 + 3);
}

fn measure(int i, Shape s) -> int {
    let m = match s {
        Shape::Circle(r) => id(r) + 1000,
        Shape::Rect(w, h) => id(w) * 100 + id(h),
    };
    return i + m;
}

fn measure_after_tuple(int i, Shape s) -> int {
    let t = (id(i), id(i + 10), id(i + 20));
    let m = match s {
        Shape::Circle(r) => id(r) * 2,
        Shape::Rect(w, h) => id(h) - id(w),
    };
    return t[0] + t[1] + t[2] + m;
}

test("dense-built payload keeps declaration order") {
    let s = pick(3);
    let w = match s {
        Shape::Circle(r) => r,
        Shape::Rect(a, b) => a * 1000 + b,
    };
    assert(w == 3031)?;
}

test("arm opening with a constant still binds its payload") {
    assert(measure(1, pick(2)) == 1 + 17 + 1000)?;
    assert(measure(1, Shape::Circle(41)) == 1042)?;
}

test("last arm reading its second field first") {
    assert(measure(1, pick(4)) == 1 + 400 + 41)?;
    assert(measure(2, Shape::Rect(6, 9)) == 2 + 609)?;
}

test("payloads above scalarized tuple slots") {
    assert(measure_after_tuple(1, pick(1)) == 1 + 11 + 21 + 20)?;
    assert(measure_after_tuple(3, pick(3)) == 3 + 13 + 23 + 28)?;
}

enum Quad {
    Four(int, int, int, int),
    Named { a: int, b: int, c: int },
}

/// Inlined calls in payload args STORE temps; earlier args stay intact.
fn quad(int i) -> int {
    let q = Quad::Four(id(i), id(i + 1), id(i + 2), id(i + 3));
    return match q {
        Quad::Four(a, b, c, d) => a * 1000 + b * 100 + c * 10 + d,
        Quad::Named { a, b, c } => a + b + c,
    };
}

fn named(int i) -> int {
    let q = Quad::Named { a: id(i), b: id(i + 1), c: id(i + 2) };
    return match q {
        Quad::Four(a, b, c, d) => a + b + c + d,
        Quad::Named { a, b, c } => a * 100 + b * 10 + c,
    };
}

test("payload args with inlined calls keep their order") {
    assert(quad(1) == 1234)?;
    assert(named(1) == 123)?;
}
