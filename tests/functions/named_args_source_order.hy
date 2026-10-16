// Named arguments run in the order they are written, not in parameter or
// field order, for calls and constructors alike (#1365).
// Output: ba3 yx12 1cb123 b2 ba1
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn t(string s, int v) -> int {
    write_all(stdout(), to_bytes(s));
    return v;
}

fn f(int a, int b) -> int {
    return a + b;
}

fn g(int a, int b, int c) -> string {
    return format("%i%i%i", a, b, c);
}

class P {
    pub x: int,
    pub y: int,
}

fn main() {
    let r = f(b: t("b", 2), a: t("a", 1));
    write_all(stdout(), to_bytes(format("%i ", r)));
    let p = new P(y: t("y", 2), x: t("x", 1));
    write_all(stdout(), to_bytes(format("%i%i ", p.x, p.y)));
    let s = g(t("1", 1), c: t("c", 3), b: t("b", 2));
    write_all(stdout(), to_bytes(format("%s ", s)));
    let n = 1;
    let q = f(b: n, a: t("b", 1));
    write_all(stdout(), to_bytes(format("%i ", q)));
    if nested() {
        write_all(stdout(), to_bytes("1"));
    }
}

fn nested() -> bool {
    // Inside a larger expression, as an operand above others.
    return 1 + f(b: t("b", 2), a: t("a", 1)) == 4;
}
