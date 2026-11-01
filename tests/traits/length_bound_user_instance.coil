// A `T: Length` bound called with a user class reaches its `impl Length`:
// `x.len()` in the mono clone calls the instance directly, as for any other
// trait, while strings, arrays and `Vec` stay structural (#1504).
use tests::assert;

class Box {
    pub n: int,
}

impl Length for Box {
    fn len(Box b) -> int {
        return 7;
    }
}

class Bag<T> {
    pub items: Vec<T>,
}

impl Length for Bag<T> {
    fn len(Bag<T> b) -> int {
        return b.items.len() + 100;
    }
}

fn size<T: Length>(T x) -> int {
    return x.len();
}

fn twice<T: Length>(T x) -> int {
    return size(x) + x.len();
}

fn main() {
    assert!(size(new Box(1)) == 7);
    assert!(twice(new Box(1)) == 14);
    assert!(size(new Bag(Vec::from([1, 2, 3]))) == 103);
    assert!(size("abc") == 3);
}
