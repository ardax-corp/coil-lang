// A record variant built with its fields named out of declaration order
// keeps each value in its declared payload slot.
enum E {
    Foo { x: int, y: string, z: int },
}

fn two() -> int {
    return 2;
}

fn mk(int a) -> E {
    return E::Foo { z: a, y: "m", x: a + 1 };
}

fn parts(E e) -> int {
    return match e {
        E::Foo { x, y, z } => x * 100 + y.len() * 10 + z,
    };
}

test("shuffled record variant fields") {
    assert(parts(E::Foo { z: 1, x: 2, y: "s" }) == 211)?;
    assert(parts(E::Foo { y: "t", z: 3, x: two() }) == 213)?;
    assert(parts(mk(4)) == 514)?;
}
