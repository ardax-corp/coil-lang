// examples/derive_hash.hy — recursive `#[derive(Hash)]` + primitive Hash.
//
// @output true,true,true,true

use io::stdout;
use io::sync::print;
use io::sync::write_all;
use string::{format, to_bytes};

#[derive(Hash, Show)]
enum Inner {
    A(int),
}

#[derive(Hash, Show)]
enum Outer {
    Wrap(Inner),
    Label { name: string, flag: bool },
}

fn main() {
    let _ = print(format("%z,", 42.hash() == 42.hash()));
    let _ = print(format("%z,", "hi".hash() == "hi".hash()));
    let _ = write_all(
        stdout(),
        to_bytes(format("%z,", Outer::Wrap(Inner::A(1)).hash() == Outer::Wrap(Inner::A(1)).hash())),
    );
    let _ = write_all(
        stdout(),
        to_bytes(format("%z", Outer::Wrap(Inner::A(1)).hash() != Outer::Wrap(Inner::A(2)).hash())),
    );
}
