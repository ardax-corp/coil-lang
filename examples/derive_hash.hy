// examples/derive_hash.hy — recursive `#[derive(Hash)]` + primitive Hash.
//
// Output: true,true,true,true

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

#[derive(Hash, Show, String)]
enum Inner {
    A(int),
}

#[derive(Hash, Show, String)]
enum Outer {
    Wrap(Inner),
    Label { name: string, flag: bool },
}

fn main() {
    write_all(stdout(), to_bytes(format("%z,", 42.hash() == 42.hash())));
    write_all(stdout(), to_bytes(format("%z,", "hi".hash() == "hi".hash())));
    write_all(
        stdout(),
        to_bytes(format("%z,", Outer::Wrap(Inner::A(1)).hash() == Outer::Wrap(Inner::A(1)).hash())),
    );
    write_all(
        stdout(),
        to_bytes(format("%z", Outer::Wrap(Inner::A(1)).hash() != Outer::Wrap(Inner::A(2)).hash())),
    );
}
