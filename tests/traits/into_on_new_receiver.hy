// `.into()` works on a `new C(...)` receiver written inline, with the
// target type chosen by the expected type.
// @output 32
use io::{stdout, write};
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
    let f: Fahrenheit = new Celsius(0).into();
    let _ = write(stdout(), to_bytes(format("%i", f.f)));
}
