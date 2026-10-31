// @expected E0107 — a `const` field is set only by `new`; not even `self.id = …` in a method.
class Ticket {
    const id: int,
}

impl Ticket {
    pub fn reset() {
        self.id = 0;
    }
}

fn main() {
    let t = new Ticket(1);
    t.reset();
}
