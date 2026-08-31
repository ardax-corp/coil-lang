// Inherent methods that return Option<T> are monomorphized; int payloads stay native.
// Contrast: free `fn f<T>(T) -> Option<T>` is E0127 (see compile_fail).
class Cell<T> {
    item: T,
}

impl Cell<T> {
    fn get() -> Option<T> {
        return Option::Some(self.item);
    }
}

test("method Option<int> payload") {
    let c = new Cell(7);
    let n = match c.get() {
        Option::Some(v) => v,
        Option::None => -1,
    };
    assert(n == 7)?;
}
