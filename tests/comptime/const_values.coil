// Consts computed at compile time keep every kind of value: scalars,
// strings, `Vec`s, class instances, records, enums and tuples.
use string::format;
use tests::assert;

fn sq(int x) -> int {
    return x * x;
}

fn squares(int n) -> Vec<int> {
    let v: Vec<int> = Vec::new();
    let i = 0;
    while i < n {
        v.push(sq(i));
        i = i + 1;
    }
    return v;
}

fn greeting(string who) -> string {
    return format("hello, %s", who);
}

class Point {
    pub x: int,
    pub y: float,
    pub name: string,
}

enum Shape {
    Circle(float),
    Rect(int, int),
    Empty,
}

fn origin() -> Point {
    return new Point(sq(3), 0.5, greeting("origin"));
}

fn pick(int n) -> Shape {
    if n > 1 {
        return Shape::Rect(n, sq(n));
    }
    return Shape::Empty;
}

fn first_even(Vec<int> xs) -> Option<int> {
    for x in xs {
        if x > 0 && x % 2 == 0 {
            return Some(x);
        }
    }
    return None;
}

const SIXTEEN = sq(4);

const TABLE = squares(6);

const HELLO = greeting("compile time");

const ORIGIN = origin();

const SHAPE = pick(3);

const NOTHING = pick(0);

const EVEN = first_even(squares(5));

const PAIR = (sq(2), greeting("pair"));

const TOTAL = SIXTEEN + TABLE[5];

test("scalars and strings") {
    assert!(SIXTEEN == 16);
    assert!(HELLO == "hello, compile time");
    assert!(TOTAL == 41);
}

test("a Vec keeps its elements") {
    assert!(TABLE.len() == 6);
    assert!(TABLE[0] == 0);
    assert!(TABLE[5] == 25);
}

test("a class instance keeps its fields") {
    assert!(ORIGIN.x == 9);
    assert!(ORIGIN.y == 0.5);
    assert!(ORIGIN.name == "hello, origin");
}

test("enums, options and tuples") {
    let rect = match SHAPE {
        Shape::Rect(w, h) => w == 3 && h == 9,
        default => false,
    };
    assert!(rect);
    let empty = match NOTHING {
        Shape::Empty => true,
        default => false,
    };
    assert!(empty);
    assert!(EVEN == Some(4));
    let (n, s) = PAIR;
    assert!(n == 4);
    assert!(s == "hello, pair");
}
