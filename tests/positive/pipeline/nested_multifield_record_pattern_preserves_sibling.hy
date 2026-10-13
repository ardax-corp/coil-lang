// Nested multi-field record patterns must not clobber sibling outer fields.
// Pre-fix, in-place `UnpackAt` at the outer field slot overwrote later
// siblings when the inner arity exceeded one.
// inner fields and outer sibling `name` must all bind correctly
// Output: 33
use io::{stdout, write};
use string::{format, to_bytes};

enum Inner {
    I { x: int, y: int },
}

enum Wrap {
    W { inner: Inner, name: int },
}

fn both(Wrap w) -> int {
    return match w {
        Wrap::W { inner: Inner::I { x, y }, name } => x + y + name,
    };
}

fn main() {
    let w = Wrap::W { inner: Inner::I { x: 10, y: 20 }, name: 3 };
    write(stdout(), to_bytes(format("%i", both(w))));
}
