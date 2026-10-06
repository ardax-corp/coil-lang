// A trait method called function-style on ground arguments goes straight
// to the instance's method, boxing the positions it unboxes.
trait Weigh<S> {
    fn weigh(S x, int k) -> int {}
}

class Crate {
    pub kg: int,
}

impl Weigh for int {
    pub fn weigh(int x, int k) -> int {
        return x * k;
    }
}

impl Weigh for Crate {
    pub fn weigh(Crate c, int k) -> int {
        return c.kg + k;
    }
}

fn heavy(Crate c) -> int {
    return weigh(c, 1) + weigh(3, 2);
}

test("ground function-style trait calls") {
    assert(weigh(4, 5) == 20)?;
    assert(heavy(new Crate(10)) == 17)?;
    assert(weigh(new Crate(2), weigh(1, 3)) == 5)?;
    let n = 7;
    assert(1 + weigh(n, n) == 50)?;
}
