// examples/record.hy — record-shaped variant payload.
//
// A `Point` enum has two variants: the unit `Origin` and a
// record-shaped `Point { x, y }` with named fields. Constructing
// and matching a record uses `{ name: value, ... }` syntax.
//
// This example demonstrates BOTH access styles:
//   - Pattern destructuring (`Point::Point { x, y } => x * x + y * y`)
//   - Field access (`p.x`, `p.y`) — added in Phase 18D.
//
// Output: the distance² from origin (5² + 12² = 169) and the
// x-coordinate of `p` (5).
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

// Phase 18D: read a record field via `p.x` instead of a pattern.
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
    // Pattern-destructured access (Phase 17B).
    write_all(stdout(), to_bytes(format("%i", distance_squared(Point::Point{ x: 5, y: 12 }))));

    // Field access (Phase 18D) — `p.x` and `p.y` extract the
    // record fields without a match.
    write_all(stdout(), to_bytes(format("%i", x_coord(Point::Point{ x: 5, y: 12 }))));
    write_all(stdout(), to_bytes(format("%i", y_coord(Point::Point{ x: 5, y: 12 }))));
}
