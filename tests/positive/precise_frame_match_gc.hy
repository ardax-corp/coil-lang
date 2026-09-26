// Frames with per-PC precise maps: match payload bindings, loop joins and
// locals must stay rooted across calls that collect.
use gc::{collect};

class Cell {
    pub v: int,
}

fn pick(int i) -> Option<Cell> {
    if i % 3 == 0 {
        return Option::None;
    }
    return Option::Some(new Cell(i));
}

fn churn() -> int {
    let i = 0;
    while i < 20 {
        let junk = new Cell(i);
        i = i + junk.v - junk.v + 1;
    }
    collect();
    return 0;
}

fn sum_picks(int n) -> int {
    let total = 0;
    let keep = [new Cell(100), new Cell(200)];
    let i = 0;
    while i < n {
        match pick(i) {
            Option::Some(c) => {
                total = total + churn();
                total = total + c.v;
            },
            Option::None => {
                total = total + churn();
            },
        };
        i = i + 1;
    }
    return total + keep[0].v + keep[1].v;
}

test("match bindings and locals survive collections in mapped frames") {
    // picks 1,2,4,5,7,8 of 0..9 sum to 27.
    assert(sum_picks(9) == 27 + 300)?;
}
