// Expected: E0413 — operators on a generic `T: Num` go through built-in traits with no
// effect declaration, so they are unknown effects.
pure fn add<T: Num>(T a, T b) -> T {
    return a + b;
}

fn main() {
    add(1, 2);
}
