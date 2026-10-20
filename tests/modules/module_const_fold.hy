// Module-level consts fold, including ones built from other consts; one
// that does not fold is computed once at startup (#1558).
use tests::assert;

const BASE = 40;

const NEXT = BASE + 2;

const NEG = 0 - NEXT - 1;

const NAME = "coil";

fn four() -> int {
    return 4;
}

const STARTUP = four() + 1;

const ITEMS = [1, 2, 3];

test("module consts fold") {
    assert!(NEXT == 42);
    assert!(NEG == -43);
    assert!(NAME == "coil");
}

test("a const that does not fold computes at startup") {
    assert!(STARTUP == 5);
    assert!(ITEMS[2] == 3);
}
