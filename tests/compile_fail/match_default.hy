// Expected: compile failure — `_` and `default` cannot both catch-all.
enum Status { Open, Closed }

fn main() {
    let s = Status::Open;
    let _ = match s {
        Status::Open => 1,
        _ => 0,
        default => 2,
    };
}
