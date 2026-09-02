// Expected: compile failure — matching a raw int on Status is not a case.
enum Status {
    Ok = 200,
    NotFound = 404,
}

fn main() {
    let s = Status::Ok;
    let _ = match s {
        200 => 1,
        default => 0,
    };
}
