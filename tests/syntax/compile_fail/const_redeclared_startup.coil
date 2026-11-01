// @expected E0109 — a `const` whose initializer runs at startup is still one declaration per name (#1960).
fn seed() -> int {
    return 3;
}

const SEED: int = seed();

const SEED: int = seed();

fn main() {
    let _ = SEED;
}
