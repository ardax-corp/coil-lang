// examples/dict.hy — anonymous records (`{ name: value, ... }`).
//
// Records are structurally typed: two `{ foo: int }` literals have the same
// type. Fields are read with `d.field`; reading a field the record does not
// have is a compile-time error (tests/compile_fail/missing_record_field.hy).
//
// Output: 4210042

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    let d = { foo: 42, bar: 100 };
    write_all(stdout(), to_bytes(format("%i", d.foo)));
    write_all(stdout(), to_bytes(format("%i", d.bar)));
    write_all(stdout(), to_bytes(format("%i", d.foo)));
}
