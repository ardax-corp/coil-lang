// Shadowing a function parameter inside a block must restore Access typing.
// Output: 42
use io::{stdout, write};
use string::{format, to_bytes};

enum A {
    A { z: int, x: int },
}

enum B {
    B { y: int },
}

fn foo(A a) {
    {
        let a = B::B { y: 7 };
    }
    write(stdout(), to_bytes(format("%i", a.x)));
}

fn main() {
    foo(A::A { z: 10, x: 42 });
}
