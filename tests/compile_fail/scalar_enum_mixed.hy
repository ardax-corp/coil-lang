// Expected: compile failure — mixed payload and `=` scalar cases.
enum Status {
    Success = 200,
    Fail(int),
}

fn main() {}
