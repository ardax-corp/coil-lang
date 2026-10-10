// `~x` is bitwise: the MIR lifting took the `NOT` opcode for the logical
// `LogNot`, so at -O0 `~5` was `false` (`coil test -O 0` runs this file).

fn flip(int a) -> int {
    return ~a;
}

fn flip_mask(int a, int mask) -> int {
    return ~a & mask;
}

test("bitwise not in a call") {
    assert(flip(5) == -6)?;
    assert(flip(0) == -1)?;
    assert(flip(-1) == 0)?;
    assert(flip_mask(12, 15) == 3)?;
}
