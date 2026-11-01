// @expected E0119 — nothing chooses the instance for `Zero::zero()`: two
// match and there is no annotation.
trait Zero<S> {
    fn zero() -> S {}
}

class Sq {
    pub w: int,
}

class Ci {
    pub r: int,
}

impl Zero for Sq {
    pub fn zero() -> Sq {
        return new Sq(0);
    }
}

impl Zero for Ci {
    pub fn zero() -> Ci {
        return new Ci(0);
    }
}

fn main() {
    let z = Zero::zero();
}
