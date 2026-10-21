// A method's bare `... xs` packs each call's own trailing arguments, as a
// free function's does (#1281): calls with different numbers and types of
// arguments each get the shape they pass, and the body can index the pack
// and take its length (coil-lang#1754).
use tests::assert;

class H {
    pub n: int,
}

impl H {
    pub fn count(... xs) -> int {
        return xs.len();
    }

    pub fn first(... xs) {
        return xs[0];
    }

    pub fn second(int skip, ... xs) {
        return xs[1];
    }

    pub fn sum(... xs) -> int {
        let total = self.n;
        let i = 0;
        while i < xs.len() {
            total += xs[i];
            i += 1;
        }
        return total;
    }

    pub fn never_called(... xs) -> int {
        return xs[9] * "only checked per call";
    }

    pub fn label(string head, ... xs) -> string {
        return head + xs[xs.len() - 1];
    }

    pub fn forward(... xs) -> int {
        return self.count(...xs) * 10;
    }
}

fn count(... xs) -> int {
    return xs.len();
}

class G<T> {
    pub v: T,
}

impl G<T> {
    pub fn size(... xs) -> int {
        return xs.len();
    }

    pub fn last(... xs) {
        return xs[xs.len() - 1];
    }
}

test("each method call packs its own shape") {
    let h = new H(1);
    assert!(count(1, "a") == 2);
    assert!(h.count(1, "a") == 2);
    assert!(h.count(1, 2) == 2);
    assert!(h.count() == 0);
    assert!(h.count(1, "a", true) == 3);
}

test("a constant index takes that element's type") {
    let h = new H(0);
    assert!(h.first(7, "x") == 7);
    assert!(h.first("y", 2) == "y");
    assert!(h.second(0, 1.5, true));
    assert!(h.second(0, true, "z") == "z");
    assert!(h.label("a", 1, "b") == "ab");
    assert!(h.label(head: "c", "d") == "cd");
}

test("a runtime index walks same-typed elements") {
    let h = new H(10);
    assert!(h.sum(1, 2, 3) == 16);
    assert!(h.sum(5) == 15);
    assert!(h.forward(1, "a", true) == 30);
    assert!(h.forward() == 0);
}

test("a generic class's method") {
    let g = new G("s");
    assert!(g.size(1, "a") == 2);
    assert!(g.size() == 0);
    assert!(g.last(1, "z") == "z");
    assert!(new G(2).last(true, 3) == 3);
}
