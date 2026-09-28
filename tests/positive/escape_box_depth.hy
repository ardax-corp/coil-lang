// Q1 / Q2 box-once: a first escape inside a loop body or `if` arm must box
// where the local is bound. Boxing inside the loop re-boxed the stale slots
// every pass, and code after the loop / `if` read the slot, not the box.
class Tally {
    pub hits: int,
}
fn bump(Tally t) -> int {
    t.hits = t.hits + 1;
    return 0;
}
test("escape inside a loop body") {
    let t = new Tally(0);
    let r = 0;
    while r < 3 {
        bump(t);
        r = r + 1;
    }
    assert(t.hits == 3)?;
}
test("escape inside one if arm") {
    let t = new Tally(0);
    let r = 1;
    if r > 0 {
        bump(t);
    }
    assert(t.hits == 1)?;
}
test("escape inside the untaken if arm") {
    let t = new Tally(5);
    let r = 0;
    if r > 0 {
        bump(t);
    }
    t.hits = t.hits + 1;
    assert(t.hits == 6)?;
}
test("escape in a nested loop") {
    let t = new Tally(0);
    let a = 0;
    while a < 2 {
        let b = 0;
        while b < 2 {
            bump(t);
            b = b + 1;
        }
        a = a + 1;
    }
    assert(t.hits == 4)?;
}
test("straight-line escape") {
    let t = new Tally(0);
    bump(t);
    bump(t);
    assert(t.hits == 2)?;
}

fn set0([int; 3] xs, int v) -> int {
    xs[0] = v;
    return 0;
}

test("stack array escapes inside a loop body") {
    let a = [1, 2, 3];
    let r = 0;
    while r < 3 {
        set0(a, a[0] + 1);
        r = r + 1;
    }
    assert(a[0] == 4)?;
}

test("stack array escapes inside one if arm") {
    let a = [7, 8, 9];
    let r = 1;
    if r > 0 {
        set0(a, 1);
    }
    assert(a[0] == 1)?;
}
