// A `static let` with a pure initializer is computed while compiling and
// stays mutable; one that cannot be computed (too long, a function value,
// reads another static) keeps its startup initializer, without an error.
use tests::assert;

fn squares(int n) -> Vec<int> {
    let v: Vec<int> = Vec::new();
    let i = 0;
    while i < n {
        v.push(i * i);
        i = i + 1;
    }
    return v;
}

fn count_to(int n) -> int {
    let i = 0;
    while i < n {
        i = i + 1;
    }
    return i;
}

fn double(int x) -> int {
    return x * 2;
}

static let table = squares(5);

static let total = 0;

static let long_run = count_to(3000000);

static let op = double;

static let copied = total + 1;

fn main() {
    assert!(table.len() == 5);
    assert!(table[4] == 16);
    table.push(25);
    assert!(table.len() == 6);
    table = squares(2);
    assert!(table.len() == 2);
    total = total + 7;
    assert!(total == 7);
    assert!(long_run == 3000000);
    let f = op;
    assert!(f(4) == 8);
    assert!(copied == 1);
}
