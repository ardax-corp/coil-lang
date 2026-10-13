// Nested multifield record that is not the first outer field must still
// relocate into scratch and preserve the preceding sibling binding.
// preceding sibling `name` and nested `x`/`y` must all bind
// Output: 33
use io::{stdout, write};
use string::{format, to_bytes};

enum Inner {
    I { x: int, y: int },
}

enum Wrap {
    W { name: int, inner: Inner },
}

fn both(Wrap w) -> int {
    return match w {
        Wrap::W { name, inner: Inner::I { x, y } } => name + x + y,
    };
}

fn main() {
    let w = Wrap::W { name: 3, inner: Inner::I { x: 10, y: 20 } };
    write(stdout(), to_bytes(format("%i", both(w))));
}
