// #1457: distinct type parameters stay distinct, but each one may still meet
// itself, an outer impl's parameter it names, or another generic's fresh one.
use tests::assert;

fn first<T, U>(T a, U b) -> T {
    return a;
}

fn second<T, U>(T a, U b) -> U {
    return b;
}

fn swap<T, U>(T a, U b) -> (U, T) {
    return (second(a, b), first(a, b));
}

class Pair<A, B> {
    pub left: A,
    pub right: B,
}

impl Pair<A, B> {
    pub fn left_or<C>(C other) -> A {
        let _ = other;
        return self.left;
    }

    pub fn with_right<C>(C right) -> Pair<A, C> {
        return new Pair(self.left, right);
    }
}

test("distinct type parameters keep their own types") {
    assert!(first(1, "x") == 1);
    assert!(second(1, "x") == "x");
    let (s, n) = swap(2, "y");
    assert!(s == "y");
    assert!(n == 2);
    let p = new Pair(3, "r");
    assert!(p.left_or(true) == 3);
    let q = p.with_right(4.5);
    assert!(q.left == 3);
    assert!(q.right == 4.5);
}
