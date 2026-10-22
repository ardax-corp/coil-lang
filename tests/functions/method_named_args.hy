// Named arguments on a method call land in their parameters after the
// receiver, and run in the order they are written, receiver first (#1380).
// @output ba112 ra112 1cb123 c1 ba12 yx133 rba1
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn t(string s, int v) -> int {
    let _ = write_all(stdout(), to_bytes(s));
    return v;
}

class C {
    pub k: int,
}

impl C {
    pub fn m(int a, int b) -> int {
        return self.k + a * 10 + b;
    }

    pub fn g(int a, int b, int c) -> string {
        return format("%i%i%i", a, b, c);
    }

    pub static fn s(int a, int b) -> int {
        return a * 10 + b;
    }

    pub fn twice(int x) -> int {
        // On `self`, inside a method.
        return self.m(b: t("y", x), a: t("x", x));
    }
}

fn make(int k) -> C {
    let _ = write_all(stdout(), to_bytes("r"));
    return new C(k);
}

fn main() {
    let c = new C(100);
    // Shuffled names with side effects run in source order.
    let r = c.m(b: t("b", 2), a: t("a", 1));
    let _ = write_all(stdout(), to_bytes(format("%i ", r)));
    // A computed receiver still runs before its arguments.
    let r2 = make(100).m(a: t("a", 1), b: 2);
    let _ = write_all(stdout(), to_bytes(format("%i ", r2)));
    // A positional prefix, then shuffled names.
    let s = c.g(t("1", 1), c: t("c", 3), b: t("b", 2));
    let _ = write_all(stdout(), to_bytes(format("%s ", s)));
    // Plain values need no temps.
    let n = 2;
    if c.m(b: n, a: 1) == 112 && c.m(1, b: t("c", 2)) == 112 && c.g(b: 2, c: 3, a: 1) == "123" {
        let _ = write_all(stdout(), to_bytes("1 "));
    }
    // A static method takes named arguments too.
    let q = C::s(b: t("b", 2), a: t("a", 1));
    let _ = write_all(stdout(), to_bytes(format("%i ", q)));
    let _ = write_all(stdout(), to_bytes(format("%i ", c.twice(3))));
    if nested() {
        let _ = write_all(stdout(), to_bytes("1"));
    }
}

fn nested() -> bool {
    // Inside a larger expression, as an operand above others.
    return 1 + make(0).m(b: t("b", 2), a: t("a", 1)) == 13;
}
