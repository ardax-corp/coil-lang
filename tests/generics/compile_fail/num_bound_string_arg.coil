// @expected E0119 — `string` has no `Num` instance (`+` on strings is not `Add`).
fn add<T: Num>(T a, T b) -> T {
    return a + b;
}

fn main() {
    let s = add("a", "b");
}
