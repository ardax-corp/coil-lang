// Expected: compile failure — Status is not constructed from a raw int.
enum Status {
    Ok = 200,
    NotFound = 404,
}

fn take_status(Status s) -> Status {
    return s;
}

fn main() {
    take_status(200);
}
