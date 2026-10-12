// A stack array read at an index the checker cannot prove in range: each
// in-range index reads its slot (out of range boxes the array and panics).
fn below(int n, int seed) -> int {
    return (seed * 7 + 3) % n;
}

fn pick_int(int seed, int big) -> int {
    let edges = [0, 1, -1, big, 0 - big];
    return edges[below(5, seed)];
}

fn pick_float(int seed) -> float {
    let xs = [0.5, 1.5, 2.5];
    return xs[below(3, seed)];
}

fn pick_name(int seed) -> string {
    let names = ["a", "bb", "ccc", "dddd"];
    return names[below(4, seed)];
}

test("ints at every slot") {
    let total = 0;
    let seed = 0;
    while seed < 10 {
        total = total + pick_int(seed, 9);
        seed = seed + 1;
    }
    assert(pick_int(0, 9) == 9)?;
    assert(pick_int(1, 9) == 0)?;
    assert(pick_int(2, 9) == -1)?;
    assert(total == 0)?;
}

test("floats and strings") {
    assert(pick_float(0) == 0.5)?;
    assert(pick_float(1) == 1.5)?;
    assert(pick_float(2) == 2.5)?;
    assert(pick_name(0) == "dddd")?;
    assert(pick_name(1) == "ccc")?;
}
