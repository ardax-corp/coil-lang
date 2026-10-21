// A method's bare `... xs` tuple pack, and named arguments with a method's
// rest parameter, pack as a free function's do (#1734).
use tests::assert;
use string::format;

class H {
    pub n: int,
}

impl H {
    pub fn tup(... xs) -> int {
        let (a, s) = xs;
        return self.n + a + s.len();
    }

    pub fn pair(int k, ... xs) -> int {
        let (a, b) = xs;
        return self.n + k + a + b;
    }

    pub fn ignore(... xs) -> int {
        return self.n;
    }

    pub fn label(string head, int... xs) -> string {
        let total = self.n;
        for x in xs {
            total = total + x;
        }
        return head + format("%i", total);
    }

    pub fn tag(string head, ... xs) -> string {
        let (b, s) = xs;
        return head + s;
    }
}

test("a method's tuple pack") {
    let h = new H(10);
    assert!(h.tup(1, "ab") == 13);
    assert!(h.tup(2, "") == 12);
    assert!(h.pair(1, 2, 3) == 16);
    assert!(h.ignore(1, "a") == 10);
    assert!(h.tag("a", true, "b") == "ab");
}

test("named arguments with a method's rest parameter") {
    let h = new H(1);
    assert!(h.label(head: "n", 1, 2) == "n4");
    assert!(h.label(head: "m") == "m1");
    let v: Vec<int> = [3, 4];
    assert!(h.label(head: "v", ...v) == "v8");
    assert!(h.tag(head: "c", false, "d") == "cd");
    assert!(h.pair(k: 1, 2, 3) == 7);
}
