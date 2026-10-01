// Bitwise operators and shifts.
test("bitwise and or xor") {
    assert((7 & 3) == 3)?;
    assert((4 | 1) == 5)?;
    assert((7 ^ 3) == 4)?;
}

test("bitwise not") {
    assert((~0) == -1)?;
    assert((~(-1)) == 0)?;
}

test("shifts") {
    assert((1 << 3) == 8)?;
    assert((16 >> 2) == 4)?;
    assert((7 << 1) == 14)?;
}

test("compound bitwise") {
    let x = 15;
    x &= 7;
    assert(x == 7)?;
    x |= 8;
    assert(x == 15)?;
    x ^= 1;
    assert(x == 14)?;
    x <<= 1;
    assert(x == 28)?;
    x >>= 2;
    assert(x == 7)?;
}

// The literal cases above fold at compile time. `opaque` hides a value from
// the optimizer (it round-trips through a `Vec`), so the cases below run the
// VM's bitwise and shift instructions.
fn opaque(int x) -> int {
    let v: Vec<int> = Vec::new();
    v.push(x);
    return v[0];
}

test("runtime and or xor") {
    assert((opaque(7) & opaque(3)) == 3)?;
    assert((opaque(4) | opaque(1)) == 5)?;
    assert((opaque(7) ^ opaque(3)) == 4)?;
    assert((opaque(-7) ^ opaque(3)) == -6)?;
}

test("runtime not") {
    assert(~opaque(0) == -1)?;
    assert(~opaque(5) == -6)?;
}

test("runtime shifts") {
    assert(opaque(1) << opaque(3) == 8)?;
    assert(opaque(1) << opaque(62) == 4611686018427387904)?;
    assert(opaque(16) >> opaque(2) == 4)?;
    assert(opaque(-16) >> opaque(2) == -4)?; // arithmetic shift
}
