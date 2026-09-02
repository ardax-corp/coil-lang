// Expected: compile failure — missing scalar case.
enum Status {
    Success = 200,
    NotFound = 404,
}

fn main() {
    let s = Status::Success;
    let _ = match s {
        Status::Success => 1,
    };
}
