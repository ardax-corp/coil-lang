// Expected: compile failure — missing scalar case.
enum Status {
    Ok = 200,
    NotFound = 404,
}

fn main() {
    let s = Status::Ok;
    let _ = match s {
        Status::Ok => 1,
    };
}
