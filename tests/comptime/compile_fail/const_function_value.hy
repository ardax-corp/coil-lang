// Expected: E0134 — a function value has no compile-time form.
class Holder {
    pub f: fn(int) -> int,
}

fn twice(int x) -> int {
    return x * 2;
}

fn make() -> Holder {
    return new Holder(twice);
}

const H = make();

fn main() {
}
