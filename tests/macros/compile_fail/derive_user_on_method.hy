// @expected E0119 — a user derive on a method is an error too (#1202).
use derive_macros::FieldNames;

class Point {
    pub x: int,
}

impl Point {
    #[derive(FieldNames)]
    pub fn get() -> int {
        return self.x;
    }
}

fn main() {}
