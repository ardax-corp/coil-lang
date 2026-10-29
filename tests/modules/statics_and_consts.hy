// Globals in bodies lowered straight from HIR to MIR: static reads
// (`LoadStatic`), writes and compound assigns (`StoreStatic`), heap
// statics (a class object, a `Vec`) and folded `const` reads, inside
// loops so the bodies reach the dense and LIR tiers.

use tests::assert;
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

const STEP = 3;

const SCALE = 1.5;

const ON = true;

const NAME = "glob";

static let counter: int = 0;

static let total: float = 0.0;

static let flag: bool = false;

static let label: string = "start";

class Acc {
    pub sum: int,
}

static let acc: Acc = new Acc(0);

static let items: Vec<int> = Vec::new();

static let last: Option<int> = Option::None;

fn bump(int n) -> int {
    let i = 0;
    while i < n {
        counter += STEP;
        i = i + 1;
    }
    return counter;
}

fn add_total(int n) -> float {
    let i = 0;
    while i < n {
        total = total + SCALE;
        i = i + 1;
    }
    return total;
}

fn reset() {
    counter = 0;
    total = 0.0;
}

fn toggle(int n) -> bool {
    let i = 0;
    while i < n {
        flag = !flag;
        i = i + 1;
    }
    return flag && ON;
}

fn read_twice(int n) -> int {
    // A call between two reads: the second read sees the write.
    let a = counter;
    bump(n);
    let b = counter;
    return b - a;
}

fn acc_sum(int n) -> int {
    // A field store needs a local base: the static's object, borrowed once.
    let a = mut acc;
    let i = 0;
    while i < n {
        a.sum = a.sum + i;
        i = i + 1;
    }
    return acc.sum;
}

fn fill(int n) -> int {
    let i = 0;
    while i < n {
        items.push(i * STEP);
        i = i + 1;
    }
    let s = 0;
    let j = 0;
    while j < items.len() {
        s = s + items[j];
        j = j + 1;
    }
    return s;
}

fn swap_acc(int v) -> int {
    acc = new Acc(v);
    return acc.sum;
}

fn relabel(string s) -> string {
    label = NAME + ":" + s;
    return label;
}

test("static int reads, writes and += in a loop") {
    reset();
    assert!(bump(4) == 12);
    assert!(bump(2) == 18);
    assert!(counter == 18);
    counter = 1;
    assert!(bump(1) == 4);
    assert!(read_twice(5) == 15);
}

test("static float and bool") {
    reset();
    assert!(add_total(4) == 6.0);
    assert!(total == 6.0);
    assert!(toggle(3));
    assert!(!toggle(1));
}

test("heap statics: a class object and a Vec") {
    assert!(acc_sum(5) == 10);
    assert!(acc_sum(3) == 13);
    assert!(swap_acc(7) == 7);
    assert!(acc_sum(2) == 8);
    assert!(fill(4) == 18);
    assert!(fill(1) == 18);
    assert!(items.len() == 5);
}

fn remember(int n) -> int {
    let i = 0;
    let seen = 0;
    while i < n {
        match last {
            Option::Some(v) => {
                seen = seen + v;
            },
            Option::None => {},
        }
        last = Option::Some(i);
        i = i + 1;
    }
    return seen;
}

fn last_or(int d) -> int {
    return match last {
        Option::Some(v) => v,
        Option::None => d,
    };
}

test("a two-word Option static") {
    assert!(last_or(-1) == -1);
    assert!(remember(4) == 3);
    assert!(last_or(-1) == 3);
    last = Option::None;
    assert!(last_or(9) == 9);
}

test("const reads and a string static") {
    assert!(relabel("x") == "glob:x");
    assert!(label == "glob:x");
    let line = format("%s %i\n", label, counter);
    write_all(stdout(), to_bytes(line))?;
}
