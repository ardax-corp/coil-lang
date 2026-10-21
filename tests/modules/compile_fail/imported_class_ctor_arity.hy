// Expected: E0204 — an imported class's constructor takes every field.
use qpath_provider::Point;

fn main() {
    let p = new Point(1);
}
