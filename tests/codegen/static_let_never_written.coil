// A `static let` nothing in the program writes reads as its initializer,
// like a `static const`. One that something writes (by assignment,
// compound assignment or `++`, in any function) keeps its slot.
use tests::assert;

static let LIMIT = 10;

static let SCALE = 2.5;

static let NAME = "coil";

static let hits = 0;

static let total = 1;

static let steps = 0;

fn hit() {
    hits = hits + 1;
}

fn grow() {
    total += 2;
}

fn step() {
    steps++;
}

test("never-written statics read as their values") {
    assert!(LIMIT * 2 == 20);
    assert!(SCALE * 2.0 == 5.0);
    assert!(NAME == "coil");
}

test("written statics keep their writes") {
    hit();
    hit();
    grow();
    step();
    assert!(hits == 2);
    assert!(total == 3);
    assert!(steps == 1);
}

test("a local named like a static does not change it") {
    let LIMIT = 3;
    LIMIT = LIMIT + 1;
    assert!(LIMIT == 4);
}
