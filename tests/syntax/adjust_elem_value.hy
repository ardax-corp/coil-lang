use tests::assert;

class Cell {
    pub v: float,
    pub n: int,
}

test("element and field adjusts used for their value after a statement adjust") {
    let xs = [5, 6, 7];
    let i = 1;
    xs[i]--;
    let a = xs[i]++;
    let b = ++xs[2];
    let c = xs[0]--;
    assert!(a == 5);
    assert!(b == 8);
    assert!(c == 5);
    assert!(xs[0] == 4 && xs[1] == 6 && xs[2] == 8);
    let cell = new Cell(1.5, 10);
    cell.v--;
    let old = cell.v++;
    let new = --cell.n;
    assert!(old == 0.5);
    assert!(cell.v == 1.5);
    assert!(new == 9 && cell.n == 9);
}
