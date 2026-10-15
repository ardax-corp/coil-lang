// Inline receiver `new Celsius(0).into()` must typecheck and run (Bugbot:
// codegen used to skip boxing when `receiver_type` only handled Identifier/Access).
// Output: 32
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
    write(stdout(), to_bytes(format("%i", f.f)));
}
