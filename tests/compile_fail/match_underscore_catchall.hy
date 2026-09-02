// Expected: compile failure — `_` is not a whole-arm catch-all.
enum Status { Open, Closed }

fn main() {
    let s = Status::Open;
    let _ = match s {
        Status::Open => 1,
        _ => 0,
    };
}
