// #1663: a `thread::spawn` worker has its own heap, and starts with a copy
// of the spawner's statics: startup `const`s and tables read the same
// values there. A `static let` starts at the spawner's value when it
// spawned; a write in the worker stays in the worker.
use tests::assert;
use thread::{join, spawn};

fn three() -> int {
    return 3;
}

const THRICE: int = three();

const WEIGHTS = readonly [1, 2, 3, 4, 5, 6, 7, 8];

const NAMES = readonly ["ada", "bo", "cy"];

const GREETING: string = "hi " + NAMES[1];

static let hits = 0;

static let log: Vec<string> = ["start"];

class Counter {
    static count: int = 10,
    value: int,
}

fn tripled(int i) -> int {
    return i * THRICE;
}

fn weighted(int i) -> int {
    return i * WEIGHTS[i % 8];
}

fn name_len(int i) -> int {
    return NAMES[i].len();
}

fn greeting_len() -> int {
    return GREETING.len();
}

fn bump() -> int {
    hits = hits + 1;
    return hits;
}

fn log_len() -> int {
    return log.len();
}

fn counter() -> int {
    return Counter::count;
}

fn sbox(int x) -> int {
    const SBOX = readonly [12, 5, 6, 11, 9, 0, 10, 13];
    return SBOX[x % 8];
}

test("a startup const reads its value in a worker") {
    assert!(join(spawn(tripled, 7)?)? == 21);
}

test("a const table reads its elements in a worker") {
    assert!(join(spawn(weighted, 5)?)? == 30);
}

test("a const array of strings reads in a worker") {
    assert!(join(spawn(name_len, 0)?)? == 3);
    assert!(join(spawn(greeting_len)?)? == 5);
}

test("a static let starts at the spawner's value, writes stay in the worker") {
    hits = 5;
    log = ["start", "more"];
    assert!(join(spawn(bump)?)? == 6);
    assert!(hits == 5);
    assert!(join(spawn(log_len)?)? == 2);
}

test("a class static reads in a worker") {
    assert!(join(spawn(counter)?)? == 10);
}

test("a local const table builds in a worker") {
    assert!(join(spawn(sbox, 3)?)? == 11);
    assert!(sbox(2) == 6);
    assert!(join(spawn(sbox, 4)?)? == 9);
}
