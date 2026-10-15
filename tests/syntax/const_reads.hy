// Module `const`s of every primitive type (including an int wider than 32
// bits and a negative literal) read back their value inside functions.
use tests::assert;

const LIMIT = 10;

const BIG = 5000000000;

const NEG = -7;

const RATE = 2.5;

const ON = true;

const TAG = "cfg";

fn scaled(int n) -> int {
    return n * LIMIT + NEG;
}

fn big_plus(int n) -> int {
    return BIG + n;
}

fn rate_of(float x) -> float {
    return x * RATE;
}

fn flag() -> bool {
    return ON && LIMIT > 3;
}

fn tagged(string s) -> string {
    return TAG + ":" + s;
}

test("const reads lower") {
    assert!(scaled(3) == 23);
    assert!(big_plus(1) == 5000000001);
    assert!(rate_of(2.0) == 5.0);
    assert!(flag());
    assert!(tagged("x") == "cfg:x");
}
