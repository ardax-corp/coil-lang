// Primitive `Hash` covers non-int payloads used by derive.
// Output: true,true,true,true,true
use io::{stdout, write};
use string::{format, to_bytes};

#[derive(Hash)]
enum Box {
    S { s: string },
    B { b: bool },
    F { f: float },
}

fn main() {
    write(stdout(), to_bytes(format("%z,", "a".hash() != "b".hash())));
    write(stdout(), to_bytes(format("%z,", true.hash() != false.hash())));
    write(stdout(), to_bytes(format("%z,", (1.0).hash() != (2.0).hash())));
    write(stdout(), to_bytes(format("%z,", Box::S { s: "x" }.hash() != Box::S { s: "y" }.hash())));
    write(
        stdout(),
        to_bytes(format("%z", Box::B { b: true }.hash() != Box::B { b: false }.hash())),
    );
}
