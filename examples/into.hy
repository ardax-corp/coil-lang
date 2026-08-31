// Expected output: 32
//
// Prelude `Into`: convert with `let y: T = x.into();`. Both Self and the
// target type must be local (strict orphan rule — builtin heads like `int`
// are not allowed as instance arguments for foreign traits).

use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};
class Celsius {
    pub c: int,
}

class Fahrenheit {
    pub f: int,
}

impl Into<Fahrenheit> for Celsius {
    fn into(Celsius x) -> Fahrenheit {
        return new Fahrenheit(x.c * 2 + 32);
    }
}

fn main() {
    let c = new Celsius(0);
    let f: Fahrenheit = c.into();
    write_all(stdout(), to_bytes(format("%i", f.f)));
}
