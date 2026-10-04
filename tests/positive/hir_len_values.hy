// `len` of a call, field or index lowers from HIR: an aggregate is
// measured with `ArrayLen`, a fixed-size value is evaluated and dropped.

class Bag {
    pub items: Vec<int>,
    pub pair: (int, int),
}

fn make(int n) -> Vec<int> {
    let out: Vec<int> = [];
    for i in 0..n {
        out.push(i);
    }
    return out;
}

fn count(Bag b) -> int {
    return len(b.items) + len(b.pair);
}

fn rows(Vec<Vec<int>> grid, int i) -> int {
    return len(grid[i]);
}

fn made(int n) -> int {
    return len(make(n)) + make(n + 1).len();
}

test("len of calls, fields and indexes lowers") {
    let b = new Bag(make(3), (1, 2));
    assert(count(b) == 5);
    let grid: Vec<Vec<int>> = [];
    grid.push(make(2));
    grid.push(make(4));
    assert(rows(grid, 1) == 4);
    assert(made(2) == 5);
}
