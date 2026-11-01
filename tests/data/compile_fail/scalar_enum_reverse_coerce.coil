// @expected E0102 — a scalar enum coerces to its backing, never back: write `Status::Ok`.
#[repr(int)]
enum Status {
    Ok = 200,
    NotFound = 404,
}

fn main() {
    let s: Status = 200;
}
