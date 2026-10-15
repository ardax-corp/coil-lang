// Expected: E0119 — scalar enums have no `.value`; they coerce to the backing implicitly.
#[repr(int)]
enum Status {
    Ok = 200,
}

fn main() {
    let n = Status::Ok.value;
}
