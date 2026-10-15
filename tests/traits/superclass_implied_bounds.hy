// A trait parameter bound declares a superclass: `impl Ordered for T`
// needs `Equal<T>`, and a `T: Ordered` bound alone makes `Equal`'s methods
// available, through bounds that are forwarded as well.
use tests::assert;

trait Equal<T> {
    fn eq_val(T a, T b) -> bool {}
}

trait Ordered<T: Equal> {
    fn lt_val(T a, T b) -> bool {}
}

class Ver {
    pub major: int,
    pub minor: int,
}

impl Equal for int {
    fn eq_val(int a, int b) -> bool {
        return a == b;
    }
}

impl Ordered for int {
    fn lt_val(int a, int b) -> bool {
        return a < b;
    }
}

impl Equal for Ver {
    fn eq_val(Ver a, Ver b) -> bool {
        return a.major == b.major && a.minor == b.minor;
    }
}

impl Ordered for Ver {
    fn lt_val(Ver a, Ver b) -> bool {
        if a.major != b.major {
            return a.major < b.major;
        }
        return a.minor < b.minor;
    }
}

// Uses the superclass method under the subclass bound only.
fn le_val<T: Ordered>(T a, T b) -> bool {
    return lt_val(a, b) || eq_val(a, b);
}

fn cmp<T: Ordered>(T a, T b) -> int {
    if eq_val(a, b) {
        return 0;
    }
    if lt_val(a, b) {
        return 0 - 1;
    }
    return 1;
}

// Forwards the bound to another generic.
fn max_of<T: Ordered>(T a, T b) -> T {
    if le_val(a, b) {
        return b;
    }
    return a;
}

test("superclass methods under a subclass bound at int") {
    assert!(le_val(1, 2));
    assert!(le_val(2, 2));
    assert!(!le_val(3, 2));
    assert!(cmp(1, 2) == 0 - 1 && cmp(2, 2) == 0 && cmp(3, 2) == 1);
}

test("superclass methods under a subclass bound at a class") {
    let a = new Ver(1, 2);
    let b = new Ver(1, 3);
    assert!(le_val(a, b));
    assert!(cmp(b, a) == 1);
    assert!(cmp(a, new Ver(1, 2)) == 0);
    assert!(max_of(a, b).minor == 3);
    assert!(max_of(new Ver(2, 0), b).major == 2);
}
