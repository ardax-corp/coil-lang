// A call to a `gen fn` emitted before the `gen fn` itself still makes a
// coroutine: the checker types it as one and codegen lowers it to
// `MakeCoro` (#787). A generic caller used to reorder the bodies.
class Holder<R> {
    pub value: Option<R>,
}

class Maker {
    id: int,
}

fn caller() -> int {
    let g = later(4);
    return resume g;
}

impl Maker {
    pub fn make<R>(unit -> R body) -> int {
        let h = new Holder(Option::None);
        let g = fill(h, body);
        return resume g;
    }
}

gen fn later(int x) -> int {
    return x + 1;
}

gen fn fill<R>(Holder<R> h, unit -> R body) -> int {
    h.value = Option::Some(body());
    return 7;
}

// Never called: a generic caller of `make` used to compile `fill` as a
// plain function for every caller.
fn unused<T>(int ms, unit -> T body) -> int {
    let m = new Maker(0);
    return m.make(fn () use (ms) {
        let x = ms;
        1
    });
}

test("free gen fn declared after its caller") {
    assert(caller() == 5)?;
}

test("generic gen fn called from a generic method") {
    let m = new Maker(3);
    assert(m.make(fn () => 2) == 7)?;
}
