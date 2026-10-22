// @expected E0413 — writing a field of an object the function did not create is `mutate`.
class Cell {
    pub v: int,
}

pure fn bump(Cell c) {
    c.v = c.v + 1;
}

fn main() {
    bump(new Cell(1));
}
