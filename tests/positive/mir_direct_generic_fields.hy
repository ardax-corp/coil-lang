// A generic class's field (`item: T`) read, written and made at a ground
// type (`Box<int>`) lowers to direct MIR: the slot holds the value's own
// word, so a `Box<int>` field reads as an `int` and a `Box<string>` one as
// a traced heap word. A two-word value (`Option<int>`) sits in the slot boxed.
use string::format;

class Box<T> {
    pub item: T,
}

class Pair<A, B> {
    pub a: A,
    pub b: B,
}

fn bump(Box<int> b) -> int {
    b.item = b.item + 1;
    return b.item * 2;
}

fn half(Box<float> b) -> float {
    return b.item / 2.0;
}

fn flag(Box<bool> b) -> bool {
    return !b.item;
}

fn label(Box<string> b) -> string {
    return b.item + "!";
}

fn inner(Box<Box<int>> b) -> int {
    return b.item.item;
}

fn some(Box<Option<int>> b) -> int {
    return b.item ?? -1;
}

fn sum(Pair<int, float> p) -> float {
    return p.a as float + p.b;
}

fn make_int(int x) -> Box<int> {
    return new Box(x + 1);
}

fn make_str(int n) -> Box<string> {
    return new Box(format("n=%v", n));
}

fn make_some(int x) -> Box<Option<int>> {
    return new Box(Option::Some(x));
}

test("int, float and bool fields") {
    let b = new Box(4);
    assert(bump(b) == 10)?;
    assert(b.item == 5)?;
    assert(half(new Box(3.0)) == 1.5)?;
    assert(flag(new Box(false)))?;
    assert(make_int(1).item == 2)?;
}

test("string fields survive allocation") {
    let boxes: Vec<Box<string>> = Vec::new();
    let i = 0;
    while i < 200 {
        boxes.push(make_str(i));
        i = i + 1;
    }
    assert(label(boxes[7]) == "n=7!")?;
    assert(boxes[199].item == "n=199")?;
}

test("nested and two-word fields") {
    assert(inner(new Box(new Box(9))) == 9)?;
    assert(some(make_some(3)) == 3)?;
    let none: Box<Option<int>> = new Box(Option::None);
    assert(some(none) == -1)?;
}

test("two parameters") {
    assert(sum(new Pair(1, 0.5)) == 1.5)?;
    let p = new Pair("k", 2);
    p.b = p.b + 1;
    assert(p.a == "k" && p.b == 3)?;
}
