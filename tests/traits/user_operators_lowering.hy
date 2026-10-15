// User operators lower to direct MIR: a trait instance method's call with
// each operand boxed, structural `==` / `!=` with no `Eq` instance, and a
// number-backed scalar enum's raw opcode on its backing word.
use tests::assert;

class V2 {
    pub x: int,
    pub y: int,
}

impl Add for V2 {
    pub fn add(V2 a, V2 b) -> V2 {
        return new V2(a.x + b.x, a.y + b.y);
    }
}

impl Eq for V2 {
    pub fn eq(V2 a, V2 b) -> bool {
        return a.x == b.x && a.y == b.y;
    }

    pub fn ne(V2 a, V2 b) -> bool {
        return !(a.x == b.x && a.y == b.y);
    }
}

#[derive(Eq)]
class Tag {
    pub name: string,
    pub n: int,
}

class Plain {
    pub n: int,
}

#[repr(int)]
enum Level {
    Low = 1,
    High = 10,
}

fn walk(int n) -> V2 {
    let acc = new V2(0, 0);
    for i in 0..n {
        acc = acc + new V2(i, 1);
    }
    return acc;
}

fn count_same(V2 a, int n) -> int {
    let hits = 0;
    for i in 0..n {
        if a == new V2(i, i) {
            hits += 1;
        }
    }
    return hits;
}

fn tags_differ(Tag a, Tag b) -> bool {
    return a != b;
}

fn plain_same(Plain a, Plain b) -> bool {
    return a == b;
}

fn levels(Level a, Level b) -> bool {
    return a == b;
}

test("instance operator calls box their operands") {
    let v = walk(5);
    assert!(v.x == 10);
    assert!(v.y == 5);
    assert!(count_same(new V2(3, 3), 6) == 1);
    assert!(new V2(1, 2) != new V2(2, 1));
}

test("derived and structural equality") {
    assert!(!tags_differ(new Tag("a", 1), new Tag("a", 1)));
    assert!(tags_differ(new Tag("a", 1), new Tag("b", 1)));
    let p = new Plain(4);
    assert!(plain_same(p, p));
}

test("scalar enum equality on the backing word") {
    assert!(levels(Level::Low, Level::Low));
    assert!(!levels(Level::Low, Level::High));
}
