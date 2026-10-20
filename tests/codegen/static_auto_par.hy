// coil-lang#793: a function that reads or writes a `static let` is not
// pure, so auto-par keeps a loop over it sequential (its workers have no
// statics). A `const` is folded and stays pure, and so does one computed at
// startup: nothing writes its slot (#1558).
use tests::assert;

static let SCALE: int = 3;

static let HITS: int = 0;

const TWICE: int = 2;

const THRICE: int = three();

fn scaled(int i) -> int {
    return i * SCALE;
}

fn counted(int i) -> int {
    HITS = HITS + 1;
    return i;
}

fn three() -> int {
    return 3;
}

fn tripled(int i) -> int {
    return i * THRICE;
}

fn doubled(int i) -> int {
    return i * TWICE;
}

test("a loop over a function that reads a static") {
    let acc = 0;
    for i in 0..400000 {
        acc = acc + scaled(i);
    }
    assert!(acc == 239999400000);
}

test("a loop over a function that writes a static") {
    let acc = 0;
    for i in 0..400000 {
        acc = acc + counted(i);
    }
    assert!(acc == 79999800000);
    assert!(HITS == 400000);
}

test("a loop over a function that reads a startup const") {
    let acc = 0;
    for i in 0..400000 {
        acc = acc + tripled(i);
    }
    assert!(acc == 239999400000);
}

test("a loop over a function that reads a const") {
    let acc = 0;
    for i in 0..400000 {
        acc = acc + doubled(i);
    }
    assert!(acc == 159999600000);
}
