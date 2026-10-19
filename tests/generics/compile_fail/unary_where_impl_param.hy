// Expected: E0119 — a method's bound on its impl's parameter goes on a
// separate bounded impl block (`impl Cell<T: Show> { … }`), not `where Show<T>`.
class Cell<T> {
    pub v: T,
}

impl Cell<T> {
    pub fn shown() -> string where Show<T> {
        return self.v.show();
    }
}

fn main() {
    let s = new Cell(1).shown();
}
