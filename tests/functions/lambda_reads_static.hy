// A lambda reads a module `static let` by its bare name, with or without
// listing it in `use (…)` (#1177).
use tests::assert;

static let N: int = 5;

static let NAME: string = "coil";

test("lambda reads a static without a capture") {
    let f = fn (int x) => x + N;
    assert!(f(1) == 6);
}

test("lambda reads a static listed in use") {
    let f = fn (int x) use (N) => x + N;
    assert!(f(2) == 7);
}

test("lambda sees a static's later value") {
    let g = fn () => N * 2;
    N = 10;
    assert!(g() == 20);
    N = 5;
}

test("lambda reads a string static") {
    let h = fn () => len(NAME);
    assert!(h() == 4);
}

test("a local shadowing a static is still a capture") {
    let N = 1;
    let f = fn (int x) use (N) => x + N;
    assert!(f(1) == 2);
}
