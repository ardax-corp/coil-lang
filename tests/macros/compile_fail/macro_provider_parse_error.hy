// @expected E0001 — the provider does not parse; each use site reports the
// provider's error, not an expansion loop (#2094).
use compile_fail::derive_macros_unparsed::{Named, Twice};

#[derive(Named)]
class A {
    pub x: int,
}

#[derive(Twice)]
class B {
    pub x: int,
}

fn main() {}
