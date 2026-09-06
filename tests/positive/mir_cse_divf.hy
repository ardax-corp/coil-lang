// COI-269: repeated DIVF in a dense kernel is CSE'd; checksum matches.
fn hot(float scale, int n) -> int {
    let i = 0;
    let s = 0.0;
    while i < n {
        let xf = i as float;
        let a = xf / scale;
        let b = xf / scale;
        s = s + a * b;
        i = i + 1;
    }
    return s as int;
}

test("cse divf matches naive sum of squares") {
    // sum_{i=0..7} (i/3)^2 = (0+1+4+9+16+25+36+49)/9 = 140/9 → 15 as int
    assert(hot(3.0, 8) == 15)?;
}
