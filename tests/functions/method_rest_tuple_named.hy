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

    pub fn two(int a, int b, int... xs) -> int {
        let s = a * 100 + b * 10;
        for x in xs {
            s = s + x;
        }
        return s;
    }
}

class G<T> {
    pub v: T,
}

impl G<T> {
    pub fn pick(... xs) -> int {
        let (a, b) = xs;
        return a + b;
    }

    pub fn two(int a, int b, int... xs) -> int {
        return a - b + xs.len();
    }
}

test("a method's tuple pack") {
    let h = new H(10);
    assert!(h.tup(1, "ab") == 13);
    assert!(h.tup(2, "") == 12);
    assert!(h.pair(1, 2, 3) == 16);
    assert!(h.ignore(1, "a") == 10);
    assert!(h.tag("a", true, "b") == "ab");
    assert!(h.tup(...(5, "ab")) == 17);
    let t = (2, "x");
    assert!(h.tup(...t) == 13);
    assert!(new G("s").pick(3, 4) == 7);
}

test("named arguments with a method's rest parameter") {
    let h = new H(1);
    assert!(h.label(head: "n", 1, 2) == "n4");
    assert!(h.label(head: "m") == "m1");
    let v: Vec<int> = [3, 4];
    assert!(h.label(head: "v", ...v) == "v8");
    assert!(h.tag(head: "c", false, "d") == "cd");
    assert!(h.pair(k: 1, 2, 3) == 7);
    assert!(h.two(b: 2, a: 1, 3, 4) == 127);
    assert!(h.two(1, b: 2) == 120);
    assert!(new G(1.5).two(b: 1, a: 5, 9) == 5);
}
