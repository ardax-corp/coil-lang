// Expected: compile failure — Status is not an int at the type level.
enum Status {
    Success = 200,
    NotFound = 404,
}

fn take_int(int n) -> int {
    return n;
}

fn main() {
    take_int(Status::Success);
}
