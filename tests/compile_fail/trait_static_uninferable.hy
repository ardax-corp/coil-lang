// Expected: compile failure — nothing chooses `T` for `make()`: two
// `Default` instances match and there is no annotation.
#[derive(Default)]
class Q { pub x: int, }
#[derive(Default)]
class R { pub y: int, }
fn make<T: Default>() -> T {
    return T::default();
}
fn main() {
    let q = make();
}
