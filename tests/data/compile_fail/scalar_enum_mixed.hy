// Expected: E0213 — mixed payload and `=` scalar cases.
enum Status {
    Ok = 200,
    Fail(int),
}

fn main() {}
