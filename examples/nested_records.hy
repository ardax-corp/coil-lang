// examples/nested_records.hy — nested record patterns.
//
// `Wrap::W { inner: Inner::I { v }, name }` destructures a record variant
// whose field is itself a record variant, binding `v` and `name` in one
// pattern.
//
// @output 99

use io::sync::print;
use string::format;

enum Inner {
    I { v: int },
}

enum Wrap {
    W { inner: Inner, name: string },
}

fn get_v(Wrap w) -> int {
    return match w {
        Wrap::W { inner: Inner::I { v }, name } => v,
    };
}

fn main() {
    let w = Wrap::W { inner: Inner::I { v: 99 }, name: "x" };
    let _ = print(format("%i", get_v(w)));
}
