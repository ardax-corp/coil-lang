// examples/dict.hy — Phase 25 dict/record demo.
//
// Demonstrates the new anonymous record (`{ name: value, ... }`)
// syntax. Dicts are STRUCTURALLY typed: two `{ foo: int }` literals
// have the same type. Fields are accessed via the existing
// `d.field` syntax. Missing-field access is a compile-time
// error (see also `dict_missing.hy`).
//
// Runtime output:
//   42
//   100
//   42

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    let d = { foo: 42, bar: 100 };
    write_all(stdout(), to_bytes(format("%i", d.foo)));
    write_all(stdout(), to_bytes(format("%i", d.bar)));
    write_all(stdout(), to_bytes(format("%i", d.foo)));
}
