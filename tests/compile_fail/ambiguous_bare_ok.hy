// Expected: compile failure — bare `Ok` is Status::Ok and Result::Ok.
enum Status {
    Ok = 200,
    NotFound = 404,
}

fn main() {
    let x = Ok(1);
}
