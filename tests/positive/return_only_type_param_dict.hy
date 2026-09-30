// A bound on a type parameter that appears only in the return type gets
// its dictionary from the call's result type, also for a nullary fn.
// (Several `Default` instances chosen by the annotation: see
// `derive_default_static.hy`.)

#[derive(Default)]
class Q {
    pub x: int,
    pub y: int,
}

fn make<T: Default>() -> T {
    return default();
}

fn make_n<T: Default>(int k) -> T {
    let _ = k;
    return default();
}

fn forwarded<T: Default>() -> T {
    return make();
}

test("nullary generic with a return-only bound") {
    let q: Q = make();
    assert(q.x == 0 && q.y == 0)?;
}

test("return-only bound with other params") {
    let q: Q = make_n(3);
    assert(q.y == 0)?;
}

test("return-only bound forwarded from a generic caller") {
    let q: Q = forwarded();
    assert(q.x == 0)?;
}
