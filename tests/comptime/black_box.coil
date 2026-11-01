// `black_box(x)` returns `x` at every one-word type, and a static let it
// initializes is computed at startup, not while compiling.
use tests::assert;

fn sq(int x) -> int {
    return x * x;
}

static let n = black_box(5);

fn main() {
    assert!(sq(black_box(7)) == 49);
    assert!(black_box("same") == "same");
    assert!(black_box(2.5) == 2.5);
    assert!(black_box(true));
    assert!(n == 5);
    let xs: Vec<int> = [1, 2];
    let v = black_box(xs);
    assert!(v.len() == 2);
}
