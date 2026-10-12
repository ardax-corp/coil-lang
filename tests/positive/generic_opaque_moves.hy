// A generic body only moves its type parameter's values: through locals,
// a loop and branches, at int, float, string, class and enum.
class Box {
    pub v: int,
}

enum Shape {
    Circle(int),
    Rect(int, int),
}

fn rotate<T>(int n, T a, T b, T c) -> T {
    let x = a;
    let y = b;
    let z = c;
    let i = 0;
    while i < n {
        let t = x;
        x = y;
        y = z;
        z = t;
        i = i + 1;
    }
    return x;
}

fn pick_last<T>(Vec<T> xs, T a, T b) -> T {
    if len(xs) > 2 {
        return a;
    }
    return b;
}

fn area(Shape s) -> int {
    return match s {
        Shape::Circle(r) => 3 * r * r,
        Shape::Rect(w, h) => w * h,
    };
}

test("ints and floats rotate through a generic loop") {
    assert(rotate(0, 1, 2, 3) == 1)?;
    assert(rotate(4, 1, 2, 3) == 2)?;
    assert(rotate(5, 1.5, 2.5, 3.5) == 3.5)?;
}

test("heap values rotate through a generic loop") {
    assert(rotate(2, "a", "b", "c") == "c")?;
    assert(rotate(1, new Box(1), new Box(2), new Box(3)).v == 2)?;
    assert(area(rotate(3, Shape::Circle(1), Shape::Rect(2, 3), Shape::Circle(2))) == 3)?;
    assert(area(rotate(1, Shape::Circle(1), Shape::Rect(2, 3), Shape::Circle(2))) == 6)?;
}

test("a generic body reads its vector's length") {
    let v: Vec<string> = Vec::new();
    v.push("x");
    assert(pick_last(v, "a", "b") == "b")?;
    v.push("y");
    v.push("z");
    assert(pick_last(v, "a", "b") == "a")?;
}
