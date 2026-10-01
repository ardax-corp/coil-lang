// examples/record.hy — record-shaped enum variants.
//
// `Point` has a unit variant `Origin` and a record variant `Point { x, y }`.
// Records are built and matched with `{ name: value, ... }`, and fields can
// also be read directly (`p.x`). Prints the squared distance from the origin
// (5² + 12² = 169), then `p.x` and `p.y`.
//
// Output: 169512

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

enum Point {
    Origin,
    Point { x: int, y: int },
}

fn distance_squared(Point p) -> int {
    return match p {
        Point::Origin => 0,
        Point::Point{ x, y } => x * x + y * y,
    };
}

// Read a record field via `p.x` instead of a pattern.
// Field access works on any value whose type is a record-shaped
// enum (here, `Point p` has the bare enum name as its declared
// type, which the typechecker resolves to the full `Ty::Sum` via
// the enum registry).
fn x_coord(Point p) -> int {
    return p.x;
}

// Field access on a different field of the same record.
fn y_coord(Point p) -> int {
    return p.y;
}

fn main() {
    // Pattern-destructured access.
    write_all(stdout(), to_bytes(format("%i", distance_squared(Point::Point{ x: 5, y: 12 }))));

    // Field access: `p.x` and `p.y` extract the
    // record fields without a match.
    write_all(stdout(), to_bytes(format("%i", x_coord(Point::Point{ x: 5, y: 12 }))));
    write_all(stdout(), to_bytes(format("%i", y_coord(Point::Point{ x: 5, y: 12 }))));
}
