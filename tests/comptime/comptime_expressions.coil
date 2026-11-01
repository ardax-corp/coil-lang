// `comptime e` computes `e` while compiling, wherever it stands: in a
// function, a method, a loop, a `const`, a `static let` or another
// `comptime` expression. It binds like unary `-`.
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

fn label(int n) -> string {
    return format("item %i", n);
}

const LIMIT = 4;

const NINE = comptime sq(3);

static let start = comptime sq(LIMIT);

class Grid {
    pub w: int,
}

impl Grid {
    pub fn cells() -> int {
        return self.w * comptime sq(LIMIT);
    }
}

fn nine_again() -> int {
    return comptime sq(3);
}

test("scalars, strings and consts") {
    assert!(comptime sq(7) == 49);
    assert!(comptime sq(7) + 1 == 50);
    assert!(comptime label(LIMIT) == "item 4");
    assert!(NINE == 9);
    assert!(comptime sq(comptime sq(2)) == 16);
    assert!(comptime (sq(2) + 1) * 2 == 10);
}

test("a Vec") {
    let v = comptime squares(5);
    assert!(v.len() == 5);
    assert!(v[4] == 16);
}

test("in a loop and a method") {
    let total = 0;
    for i in 0..3 {
        total = total + comptime sq(LIMIT) + i;
    }
    assert!(total == 51);
    let g = new Grid(2);
    assert!(g.cells() == 32);
}

test("a static let starts from it and stays mutable") {
    assert!(start == 16);
    start = start + 1;
    assert!(start == 17);
}

test("a function that reads one can be computed too") {
    assert!(comptime nine_again() == 9);
}
