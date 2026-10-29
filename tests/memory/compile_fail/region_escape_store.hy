// @expected E0140 — a region's object stored into an object made before it.
use string::format;

class Holder {
    pub v: Option<string>,
}

fn main() {
    let outer = new Holder(Option::None);
    region {
        let inner = new Holder(Option::None);
        inner.v = Option::Some(format("ok%i", 1));
        outer.v = Option::Some(format("c%i", 3));
    }
}
