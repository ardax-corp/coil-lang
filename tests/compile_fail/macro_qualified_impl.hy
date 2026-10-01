// Expected: E0119 — a module-qualified `impl` head is only written by macros (hygiene);
// source imports the trait and names it bare.
use derive_macros::Summary;

class Point {
    pub x: int,
}

impl derive_macros::Summary for Point {
    pub fn summary(Point self) -> string {
        return "Point";
    }
}

fn main() {}
