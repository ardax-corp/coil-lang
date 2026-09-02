// Expected: compile failure — mixed payload and `=` scalar cases.
enum Status {
    Ok = 200,
    Fail(int),
}

fn main() {}
