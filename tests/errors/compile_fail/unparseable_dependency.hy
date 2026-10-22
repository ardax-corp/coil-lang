// @expected E0001 — `mod` loads a dependency that does not parse.
mod compile_fail::ns_unparseable;

fn main() {}
