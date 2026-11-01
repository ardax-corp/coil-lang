// A trait method with a default body (`next`) that calls a required one
// (`base`).
//
// @output 42

use io::sync::print;
use string::format;

trait Tiny<T> {
    fn base(T x) -> int {}

    fn next(T x) -> int {
        return base(x) + 1;
    }
}

impl Tiny for int {
    pub fn base(int x) -> int {
        return x;
    }
}

fn get<T: Tiny>(T x) -> int {
    return next(x);
}

fn main() {
    let _ = print(format("%i", get(41)));
}
