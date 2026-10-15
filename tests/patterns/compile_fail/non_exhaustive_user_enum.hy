// Expected: E0209 — non-exhaustive match on user-defined enum.
enum Status { Open, Closed }

fn main() {
    let s = Status::Open;
    let _ = match s {
        Status::Open => 1,
    };
}
