// Lowering reads a local where it lives instead of copying it first.
enum Shape {
    Circle(int),
    Rect(int, int),
}

fn range_end_local(int n) -> int {
    let hi = n;
    let s = 0;
    for i in 0..hi {
        s = s + i;
    }
    return s;
}

// The body moves the end: the loop still stops where it started.
fn range_end_moves(int n) -> int {
    let hi = n;
    let s = 0;
    for i in 0..hi {
        hi = hi + 1;
        s = s + 1;
    }
    return s;
}

fn stack_store(int n) -> int {
    let xs = [0, 0, 0];
    let i = 0;
    while i < n {
        let v = i * 2;
        xs[i % 3] = v;
        i = i + 1;
    }
    return xs[0] + xs[1] + xs[2];
}

fn staged(int n) -> int {
    let t = n;
    return t + match Option::Some(n + 1) {
        Option::Some(x) => x,
        Option::None => 0,
    };
}

fn scalar_match(int k) -> int {
    return match k {
        1 => 10,
        2 => 20,
        v => v * 100,
    };
}

fn arm_writes(int i) -> int {
    let s = Shape::Rect(i, 3);
    if i > 5 {
        s = Shape::Circle(i);
    }
    return match s {
        Shape::Circle(r) => r,
        Shape::Rect(w, h) => {
            w = w + h;
            w * h
        },
    };
}

fn unread_counter() -> int {
    let total = 0;
    let i = 0;
    while i < 3 {
        let j = 0;
        while j < 3 {
            total = total + 1;
            j = j + 1;
        }
        i = i + 1;
    }
    return total;
}

test("locals read in place") {
    assert(range_end_local(5) == 10)?;
    assert(range_end_moves(4) == 4)?;
    assert(stack_store(5) == 18)?;
    assert(staged(3) == 7)?;
    assert(scalar_match(2) == 20)?;
    assert(scalar_match(7) == 700)?;
    assert(arm_writes(2) == 15)?;
    assert(arm_writes(9) == 9)?;
    assert(unread_counter() == 9)?;
}
