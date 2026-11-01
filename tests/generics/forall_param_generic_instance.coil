// A bounded generic passed to a `forall` parameter whose own signature
// grounds the bound at a fixed array (`Length<[int; 3]>`, the shared
// `[T]` instance's) or a generic instance's type (`Sz<Box<int>>`, of
// `impl Sz for Box<T>`) is accepted (#1542).
use tests::assert;

class Box<T> {
    pub v: T,
}

trait Sz<T> {
    fn sz(T x) -> int {}
}

impl Sz for Box<T> {
    fn sz(Box<T> b) -> int {
        return 1;
    }
}

fn shown_len<A: Show, B: Length>(A a, B b) -> int {
    return show(a).len() + b.len();
}

fn shown_sz<A: Show, B: Sz>(A a, B b) -> int {
    return show(a).len() + b.sz();
}

fn apply_arr(forall T: Show. T -> [int; 3] -> int f) -> int {
    return f(12, [1, 2, 3]) + f("a", [4, 5, 6]);
}

fn apply_box(forall T: Show. T -> Box<int> -> int f) -> int {
    return f(12, new Box(1));
}

test("a fixed array meets the shared [T] Length instance") {
    assert!(apply_arr(shown_len) == 9);
}

test("a generic instance meets a rank-n argument's bound") {
    assert!(apply_box(shown_sz) == 3);
}
