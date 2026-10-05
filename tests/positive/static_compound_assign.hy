static let hits: int = 0;

static let total: int = 0;

static let fl: float = 1.5;

static let log: string = "";

static let shade: int = 7;

class Counter {
    pub static count: int = 0,
}

class Cell {
    pub v: float,
}

fn compound(int n) -> int {
    total += n;
    total -= 1;
    total *= 2;
    return total;
}

fn incr() -> int {
    hits++;
    let a = hits++;
    let b = ++hits;
    hits--;
    let c = hits--;
    return a * 100 + b * 10 + c;
}

fn shadowed(int shade) -> int {
    shade += 100;
    shade++;
    return shade;
}

test("static compound assign") {
    total = 0;
    assert(compound(3) == 4)?;
    assert(total == 4)?;
}

test("static increment and decrement") {
    hits = 0;
    assert(incr() == 132)?;
    assert(hits == 1)?;
}

test("float static compound assign") {
    fl *= 2.0;
    fl++;
    fl--;
    fl++;
    assert(fl == 4.0)?;
}

test("string static append") {
    log += "a";
    log += "b";
    assert(log == "ab")?;
}

test("param shadows static") {
    assert(shadowed(1) == 102)?;
    assert(shade == 7)?;
}

test("class static compound assign") {
    Counter::count += 3;
    Counter::count++;
    assert(Counter::count == 4)?;
}

test("decrement heap elements and float fields") {
    let xs = [5, 6, 7];
    let i = 1;
    xs[i]--;
    xs[i + 1]--;
    assert(xs[1] == 5)?;
    assert(xs[2] == 6)?;
    let b = new Cell(1.5);
    b.v++;
    b.v--;
    b.v++;
    assert(b.v == 2.5)?;
    let old = b.v++;
    assert(old == 2.5)?;
    assert(b.v == 3.5)?;
    let i0 = xs[i]++;
    assert(i0 == 5)?;
    assert(xs[1] == 6)?;
}

test("postfix value of a stack array element") {
    let xs = [5, 6, 7];
    let i = 1;
    let i0 = xs[i]++;
    assert(i0 == 6)?;
    assert(xs[1] == 7)?;
}
