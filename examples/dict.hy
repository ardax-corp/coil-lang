// examples/dict.hy — anonymous records (`{ name: value, ... }`).
//
// Records are structurally typed: two `{ foo: int }` literals have the same
// type. Fields are read with `d.field`; reading a field the record does not
// have is a compile-time error (tests/compile_fail/missing_record_field.hy).
//
// @output 4210042

use io::sync::print;
use string::format;

fn main() {
    let d = { foo: 42, bar: 100 };
    let _ = print(format("%i", d.foo));
    let _ = print(format("%i", d.bar));
    let _ = print(format("%i", d.foo));
}
