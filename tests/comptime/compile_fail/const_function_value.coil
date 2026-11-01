// @expected E0134 — a `comptime` expression's value must exist while
// compiling, and a function value has no compile-time form.
class Holder {
    pub f: fn(int) -> int,
}

fn twice(int x) -> int {
    return x * 2;
}

fn make() -> Holder {
    return new Holder(twice);
}

fn main() {
    let h = comptime make();
}
