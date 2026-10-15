// Expected: E0214 — two scalar cases may not share a backing value.
enum Status {
    Ok = 200,
    Fine = 200,
}

fn main() {
}
