// Module-level consts fold, including ones built from other consts, and a
// value computed at startup is a `static const`.
const BASE = 40;

const NEXT = BASE + 2;

const NEG = 0 - NEXT - 1;

const NAME = "coil";

fn four() -> int {
    return 4;
}

static const STARTUP = four() + 1;

static const ITEMS = [1, 2, 3];

test("module consts fold") {
    assert(NEXT == 42)?;
    assert(NEG == -43)?;
    assert(NAME == "coil")?;
}

test("static const computes at startup") {
    assert(STARTUP == 5)?;
    assert(ITEMS[2] == 3)?;
}
