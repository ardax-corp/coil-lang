// Expected: E0216 — `_` is not a catch-all (`default` is).
enum Status { Open, Closed }

fn main() {
    let s = Status::Open;
    let _ = match s {
        Status::Open => 1,
        _ => 0,
        default => 2,
    };
}
