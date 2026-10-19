// A field of a class instance held in a `static let` is written in place:
// later reads of the static see it (#1557).
use tests::assert;

class P {
    pub x: int,
    pub inner: Q,
}

class Q {
    pub y: int,
}

static let G: P = new P(0, new Q(1));

fn bump() {
    G.x = G.x + 1;
}

test("a static class instance's fields are writable") {
    G.x = 9;
    assert!(G.x == 9);
    bump();
    assert!(G.x == 10);
    G.x += 5;
    assert!(G.x == 15);
    G.inner.y = 7;
    assert!(G.inner.y == 7);
    let g = G;
    assert!(g.x == 15);
}
