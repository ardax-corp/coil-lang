// A default trait method reached from a monomorphized generic still gets the
// instance dictionary it uses to call its sibling (`base`).
trait Tiny<T> {
    fn base(T x) -> int;

    fn next(T x) -> int {
        return base(x) + 1;
    }
}

impl Tiny for int {
    pub fn base(int x) -> int {
        return x;
    }
}

fn get<T: Tiny>(T x) -> int {
    return next(x);
}

test("default method via generic call") {
    assert(get(41) == 42)?;
}
