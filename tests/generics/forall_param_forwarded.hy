// A `forall` parameter passes on to another `forall` parameter of the same
// type (#1189).
// @output 5 x 8
use io::{stdout, write};
use string::{format, to_bytes};

fn id<T>(T x) -> T {
    return x;
}

fn app(forall T. T -> T f, int x) -> int {
    return f(x);
}

fn relay(forall T. T -> T f, int x) -> int {
    return app(f, x) + 1;
}

fn relay_s(forall T. T -> T f, string s) -> string {
    return f(s);
}

fn twice(forall T. T -> T f, int x) -> int {
    return relay(f, relay(f, x));
}

fn main() {
    let n = relay(id, 4);
    let s = relay_s(id, "x");
    let t = twice(id, 6);
    let _ = write(stdout(), to_bytes(format("%i %s %i\n", n, s, t)));
}
