// #1266: an element adjust or compound assignment used as a statement,
// whose index has an effect, runs the index once and updates that element.
use tests::assert;

class Counter {
    pub hits: int,
}

class Holder {
    pub items: Vec<int>,
}

fn next_index(Counter c) -> int {
    c.hits += 1;
    return 1;
}

fn at(Counter c, int i) -> int {
    c.hits += 1;
    return i;
}

test("decrement at a call index") {
    let c = new Counter(0);
    let xs = [1, 2, 3];
    xs[next_index(c)]--;
    assert!(c.hits == 1);
    assert!(xs[1] == 1);
}

test("increment and compound assignment at a call index") {
    let c = new Counter(0);
    let xs = [1, 2, 3];
    xs[next_index(c)]++;
    xs[next_index(c)] += 5;
    xs[next_index(c)] *= 2;
    assert!(c.hits == 3);
    assert!(xs[1] == 16);
    assert!(xs[0] == 1);
    assert!(xs[2] == 3);
}

test("nested indices run once each, in order") {
    let c = new Counter(0);
    let grid = [[1, 2], [3, 4]];
    grid[at(c, 1)][at(c, 0)] += 10;
    assert!(c.hits == 2);
    assert!(grid[1][0] == 13);
    assert!(grid[0][0] == 1);
}

test("an index into a field") {
    let c = new Counter(0);
    let h = new Holder([5, 6, 7]);
    h.items[at(c, 2)]--;
    assert!(c.hits == 1);
    assert!(h.items[2] == 6);
}
