// Memory: 100k rows, each an array literal of three objects stored in a Vec;
// half the rows are dropped, then churn on top. Literal elements carry a
// pointer element kind (`MakeArrayK`), so marking treats them as precise
// references and evacuation may move the objects they hold.
use gc::{collect};
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

class Cell {
    pub id: int,
    pub weight: int,
}

fn row(int i) -> [Cell; 3] {
    return [new Cell(i, 1), new Cell(i + 1, 2), new Cell(i + 2, 3)];
}

fn weigh(Vec<[Cell; 3]> rows) -> int {
    let total = 0;
    let i = 0;
    while i < len(rows) {
        let r = rows[i];
        total = total + r[0].weight + r[1].weight + r[2].id % 7;
        i = i + 1;
    }
    return total;
}

// Build 100k rows, keep every other one (the dropped half fragments the
// heap; the full list dies when this returns).
fn thinned() -> Vec<[Cell; 3]> {
    let rows: Vec<[Cell; 3]> = Vec::new();
    let i = 0;
    while i < 100000 {
        rows.push(row(i));
        i = i + 1;
    }
    let kept: Vec<[Cell; 3]> = Vec::new();
    let j = 0;
    while j < len(rows) {
        if j % 2 == 0 {
            kept.push(rows[j]);
        }
        j = j + 1;
    }
    return kept;
}

// Collect from a callee: the caller's frame then has a precise map.
fn settle() {
    collect();
}

fn main() {
    let kept = thinned();
    settle();
    let total = 0;
    let round = 0;
    while round < 32 {
        let junk = 0;
        while junk < 20000 {
            let t = new Cell(junk, junk);
            junk = junk + t.weight - t.id + 1;
        }
        total = total + weigh(kept);
        settle();
        round = round + 1;
    }
    write_all(stdout(), to_bytes(format("%i", total)));
}
