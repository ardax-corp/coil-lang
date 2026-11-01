// #1561: a `const` set to a `readonly` array literal is a lookup table. A
// module table is built once at startup and a function-local one on its
// first evaluation; reads at a constant index fold to the element. What
// each read sees is the same as for an array built at every evaluation.
use tests::assert;

const SQUARES = readonly [0, 1, 4, 9, 16, 25, 36, 49];

const HALVES = readonly [0.5, 1.5, 2.5];

const FLAGS = readonly [true, false, true];

const NAMES = readonly ["zero", "one", "two"];

const BASE = 100;

// Elements that fold against another const, and one that does not.
const OFFSETS = readonly [BASE, BASE + 1, ten()];

fn ten() -> int {
    return 10;
}

fn square(int i) -> int {
    return SQUARES[i];
}

fn sum_squares(int n) -> int {
    let s = 0;
    for i in 0..n {
        s += SQUARES[i % SQUARES.len()];
    }
    return s;
}

fn nibble_bits(int x) -> int {
    const BITS = readonly [0, 1, 1, 2, 1, 2, 2, 3, 1, 2, 2, 3, 2, 3, 3, 4];
    return BITS[x & 15] + BITS[(x >> 4) & 15];
}

fn digits() -> readonly [int; 3] {
    const D = readonly [7, 8, 9];
    return D;
}

fn first_and_len() -> int {
    const D = readonly [7, 8, 9];
    return D[0] * 10 + D.len();
}

fn word(int i) -> string {
    const WORDS = readonly ["red", "green", "blue"];
    return WORDS[i];
}

fn pick_in_loop(int n) -> int {
    let s = 0;
    for i in 0..n {
        const STEP = readonly [3, 1, 4, 1, 5];
        s += STEP[i % 5];
    }
    return s;
}

test("module table reads at constant and variable indices") {
    assert!(SQUARES[3] == 9);
    assert!(SQUARES.len() == 8);
    assert!(square(5) == 25);
    let i = 7;
    assert!(SQUARES[i] == 49);
    assert!(sum_squares(20) == 2 * (0 + 1 + 4 + 9 + 16 + 25 + 36 + 49) + 0 + 1 + 4 + 9);
}

test("module tables of floats, bools and strings") {
    assert!(HALVES[1] == 1.5);
    assert!(HALVES[0] + HALVES[2] == 3.0);
    assert!(FLAGS[0] && !FLAGS[1]);
    assert!(NAMES[2] == "two");
    assert!(NAMES.len() == 3);
}

test("a module table with elements computed at startup") {
    assert!(OFFSETS[0] == 100);
    assert!(OFFSETS[1] == 101);
    assert!(OFFSETS[2] == 10);
    let k = 2;
    assert!(OFFSETS[k] + OFFSETS[0] == 110);
}

test("a module table read through a local") {
    let t = SQUARES;
    assert!(t[4] == 16);
    assert!(t == SQUARES);
}

test("a local table read with a variable index") {
    let s = 0;
    for x in 0..256 {
        s += nibble_bits(x);
    }
    assert!(s == 1024);
    assert!(nibble_bits(255) == 8);
}

test("a local table that escapes is the same value at every call") {
    let a = digits();
    let b = digits();
    assert!(a == b);
    assert!(a[0] + a[1] + a[2] == 24);
    assert!(b == [7, 8, 9]);
}

test("local table reads at constant indices") {
    assert!(first_and_len() == 73);
    assert!(first_and_len() == 73);
}

test("a local table of strings") {
    assert!(word(1) == "green");
    assert!(word(2) + word(0) == "bluered");
}

test("a local table declared inside a loop") {
    assert!(pick_in_loop(10) == 2 * (3 + 1 + 4 + 1 + 5));
}
