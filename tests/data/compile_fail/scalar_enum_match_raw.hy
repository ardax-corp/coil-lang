// Expected: E0102 — a `Status` scrutinee matches `Status::…` cases, not raw backing literals.
#[repr(int)]
enum Status {
    Ok = 200,
    NotFound = 404,
}

fn f(Status s) -> int {
    return match s {
        200 => 1,
        default => 0,
    };
}

fn main() {
    let n = f(Status::Ok);
}
