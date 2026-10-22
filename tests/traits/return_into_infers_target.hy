// `return c.into();` under `-> Fahrenheit` pins the Into target at runtime.
// @output 32
use io::{stdout, write};
use string::{format, to_bytes};

class Celsius {
    pub c: int,
}

class Fahrenheit {
    pub f: int,
}

class Kelvin {
    pub k: int,
}

impl Into<Fahrenheit> for Celsius {
    fn into(Celsius x) -> Fahrenheit {
        return new Fahrenheit(x.c * 2 + 32);
    }
}

impl Into<Kelvin> for Celsius {
    fn into(Celsius x) -> Kelvin {
        return new Kelvin(x.c);
    }
}

fn to_f(Celsius c) -> Fahrenheit {
    return c.into();
}

fn main() {
    let f = to_f(new Celsius(0));
    let _ = write(stdout(), to_bytes(format("%i", f.f)));
}
