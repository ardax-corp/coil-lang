// Inner-block destructure must not clobber an outer binding's slot.
// outer `a` must survive inner-block shadow
// Output: 42
use io::{stdout, write};
use string::{format, to_bytes};

enum A {
    A { z: int, x: int },
}

enum B {
    B { y: int },
}

fn main() {
    let outer = { a: A::A { z: 10, x: 42 } };
    let { a } = outer;
    {
        let inner = { a: B::B { y: 7 } };
        let { a } = inner;
    }
    let _ = write(stdout(), to_bytes(format("%i", a.x)));
}
