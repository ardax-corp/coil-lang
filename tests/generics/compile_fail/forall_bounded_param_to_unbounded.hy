// Expected: E0119 — a `forall T: Show.` parameter cannot be passed where an unbounded `forall T.` is expected (#1189).
fn app(forall T. T -> string f, int x) -> string {
    return f(x);
}

fn relay(forall T: Show. T -> string f, int x) -> string {
    return app(f, x);
}

fn main() {
    let n = 1;
}
