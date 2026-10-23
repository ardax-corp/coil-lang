//
// Prelude `Into`: convert with `let y: T = x.into();`. Both Self and the
// target type must be local (strict orphan rule — builtin heads like `int`
// are not allowed as instance arguments for foreign traits).
//
// @output 32

use io::sync::print;
use string::format;

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
    let _ = print(format("%i", f.f));
}
