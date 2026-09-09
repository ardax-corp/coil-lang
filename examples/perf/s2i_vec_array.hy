// S2i: observed/escape zip stays a heap object (Index/StoreIndex).
// Checksums match examples/vec_array.hy (46 / 45 / 18).

fn zip_sum() -> int {
    let a = [1, 2] + [3, 4];
    return a[0] + a[1];
}

fn from_slots() -> int {
    let xs = [1, 2];
    let ys = [3, 4];
    let a = xs + ys;
    return a[0] + a[1];
}

fn poke() -> int {
    let a = [1, 2] + [3, 4];
    a[0] = 9;
    return a[0] + a[1];
}

fn broadcast() -> int {
    let b = [1, 2] + 3;
    return b[0] * 10 + b[1];
}

fn pow2() -> int {
    let c = [1, 2] ** 3;
    return c[0] * 10 + c[1];
}

fn escape() -> [int; 2] {
    return [1, 2] + [3, 4];
}

fn main() {
    if zip_sum() != 10 {
        panic "s2i zip";
    }
    if from_slots() != 10 {
        panic "s2i slots";
    }
    if poke() != 15 {
        panic "s2i StoreIndex";
    }
    if broadcast() != 45 {
        panic "s2i broadcast";
    }
    if pow2() != 18 {
        panic "s2i pow";
    }
    let e = escape();
    if e[0] * 10 + e[1] != 46 {
        panic "s2i escape";
    }
}
