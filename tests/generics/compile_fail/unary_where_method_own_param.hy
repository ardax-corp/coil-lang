// @expected E0119 — a method's own type parameter takes its bound inline
// (`fn show_it<U: Show>`), not as `where Show<U>`.
class Cell<T> {
    pub v: T,
}

impl Cell<T> {
    pub fn show_it<U>(U x) -> string where Show<U> {
        return x.show();
    }
}

fn main() {
    let s = new Cell(1).show_it(2);
}
