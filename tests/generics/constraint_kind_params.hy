// A constraint-kind parameter (`c: * -> Constraint`, `T: c`) is pinned to
// the trait whose method the body uses; superclass methods of that trait
// are then available, and callers pass any type with an instance.
use tests::assert;

trait Equal<T> {
    fn eq_val(T a, T b) -> bool {}
}

trait Ordered<T: Equal> {
    fn lt_val(T a, T b) -> bool {}
}

class Ver {
    pub n: int,
}

impl Equal for int {
    pub fn eq_val(int a, int b) -> bool {
        return a == b;
    }
}

impl Ordered for int {
    pub fn lt_val(int a, int b) -> bool {
        return a < b;
    }
}

impl Equal for Ver {
    pub fn eq_val(Ver a, Ver b) -> bool {
        return a.n == b.n;
    }
}

impl Ordered for Ver {
    pub fn lt_val(Ver a, Ver b) -> bool {
        return a.n < b.n;
    }
}

fn compare<c: * -> Constraint, T: c>(T a, T b) -> int {
    if lt_val(a, b) {
        return 0 - 1;
    }
    if eq_val(a, b) {
        return 0;
    }
    return 1;
}

fn same<c: * -> Constraint, T: c>(T a, T b) -> bool {
    return eq_val(a, b);
}

test("pinned to Ordered, with the Equal superclass") {
    assert!(compare(1, 2) == 0 - 1);
    assert!(compare(2, 2) == 0);
    assert!(compare(3, 2) == 1);
}

test("at a user class") {
    assert!(compare(new Ver(1), new Ver(1)) == 0);
    assert!(compare(new Ver(5), new Ver(1)) == 1);
}

test("pinned to Equal") {
    assert!(same(4, 4));
    assert!(!same(new Ver(1), new Ver(2)));
}
