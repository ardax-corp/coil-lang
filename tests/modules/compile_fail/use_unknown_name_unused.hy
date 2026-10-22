// @expected E0100 — `use math::nosuch` names nothing `math` declares. The
// `use` itself is the error, even when nothing reads `nosuch` (#1851).
use math::nosuch;

fn main() {}
