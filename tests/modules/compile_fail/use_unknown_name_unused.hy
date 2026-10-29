// @expected E0100 — `use calc::nosuch` names nothing `calc` declares. The
// `use` itself is the error, even when nothing reads `nosuch` (#1851).
use calc::nosuch;

fn main() {}
