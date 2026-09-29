// Memory: 200k live four-field objects (two ints, two refs). Typed fields are
// raw words stored inside the object, so four fit without a spill `Vec`.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

class Cell {
    pub id: int,
    pub weight: int,
    pub left: Option<Cell>,
    pub right: Option<Cell>,
}

fn build(int n) -> Vec<Cell> {
    let cells: Vec<Cell> = Vec::with_capacity(n);
    let prev: Option<Cell> = Option::None;
    let i = 0;
    while i < n {
        let c = new Cell(i, i * 3, prev, Option::None);
        cells.push(c);
        prev = Option::Some(c);
        i = i + 1;
    }
    return cells;
}

fn weigh(Vec<Cell> cells) -> int {
    let total = 0;
    let i = 0;
    while i < len(cells) {
        let c = cells[i];
        total = total + c.weight - c.id;
        total = total + match c.left {
            Option::Some(l) => l.id % 7,
            Option::None => 0,
        };
        i = i + 1;
    }
    return total;
}

fn main() {
    let total = 0;
    let round = 0;
    while round < 4 {
        total = total + weigh(build(200000));
        round = round + 1;
    }
    write_all(stdout(), to_bytes(format("%i", total)));
}
