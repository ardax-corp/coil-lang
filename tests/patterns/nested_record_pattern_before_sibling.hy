// A nested multi-field record pattern before a sibling field binds both the
// nested fields and the sibling.
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
    let _ = write(stdout(), to_bytes(format("%i", both(w))));
}
