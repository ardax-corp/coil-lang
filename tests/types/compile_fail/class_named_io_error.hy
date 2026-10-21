// Expected: E0200 — `IoError` is a built-in error enum with a built-in
// `Show` instance; a class cannot take the name (#1701).
class IoError {
    pub n: int,
}

impl Show for IoError {
    pub fn show(IoError e) -> string {
        return "x";
    }
}

fn get(IoError e) -> int {
    return e.n;
}

fn main() {
    let _ = get(new IoError(3));
}
