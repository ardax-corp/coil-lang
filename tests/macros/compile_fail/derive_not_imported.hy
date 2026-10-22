// @expected E0119 — a user derive must be imported with `use` before
// `#[derive(…)]` can name it.
#[derive(FieldNames)]
class C {
    pub x: int,
}

fn main() {}
