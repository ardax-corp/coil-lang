// @expected E0119 — `typeof` needs a ground type; an open `T` has none.
fn name_of<T>(T x) -> string {
    return typeof x;
}

fn main() {
    name_of(1);
}
