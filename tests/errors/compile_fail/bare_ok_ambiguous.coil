// @expected E0201 — a user `Status::Ok` makes bare `Ok` ambiguous; write `Result::Ok`.
#[repr(int)]
enum Status {
    Ok = 200,
    NotFound = 404,
}

fn f() -> Result<int, string> {
    return Ok(1);
}

fn main() {
    let r = f();
}
