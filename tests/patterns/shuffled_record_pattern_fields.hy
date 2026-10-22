// Shuffled record pattern `{ y: _, x: a }` must bind declaration-order `x`.
// @output 1
use io::{stdout, write};
use string::{format, to_bytes};

enum E {
    Foo { x: int, y: int, z: int },
}

fn main() {
    let e = E::Foo { x: 1, y: 2, z: 3 };
    let v = match e {
        E::Foo { y: _, x: a, z: _ } => a,
    };
    let _ = write(stdout(), to_bytes(format("%i", v)));
}
