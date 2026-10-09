// Loop-invariant code moves in front of its loop, and only where moving
// it cannot change what the program does: a loop that runs no iteration
// computes nothing, and code an iteration may skip stays put.
fn checked(int k) -> int {
    let a = [1, 2, 3];
    return a[k];
}

fn nested(int n) -> int {
    let s = 0;
    let y = 0;
    while y < n {
        let x = 0;
        while x < n {
            s = s + (n * 3 + 1) + y * 2;
            x = x + 1;
        }
        y = y + 1;
    }
    return s;
}

fn zero_trip(int n, int k) -> int {
    let s = 0;
    let i = 0;
    while i < n {
        s = s + checked(k);
        i = i + 1;
    }
    return s;
}

fn skipped(int n, int k) -> int {
    let s = 0;
    let i = 0;
    while i < n {
        if k < 3 {
            s = s + checked(k) + 100 / (k + 1);
        }
        i = i + 1;
    }
    return s;
}

fn after_exit(int n, int k) -> int {
    let s = 0;
    let i = 0;
    while true {
        if i >= n {
            break;
        }
        s = s + checked(k);
        i = i + 1;
    }
    return s;
}

fn changes(int n) -> int {
    let s = 0;
    let m = 1;
    let i = 0;
    while i < n {
        s = s + m * 10;
        m = m + 1;
        i = i + 1;
    }
    return s;
}

fn over([int] xs, int k) -> float {
    let s = 0.0;
    for x in xs {
        s = s + (k as float) * 0.5 + (x as float);
    }
    return s;
}

test("invariant arithmetic in nested loops") {
    assert(nested(3) == 3 * 3 * 10 + 3 * (0 + 2 + 4))?;
}

test("a loop that runs no iteration calls nothing") {
    assert(zero_trip(0, 7) == 0)?;
    assert(zero_trip(2, 1) == 4)?;
}

test("code an iteration skips is not computed early") {
    assert(skipped(4, 9) == 0)?;
    assert(skipped(2, 1) == 2 * (2 + 50))?;
}

test("code after an exit is not computed early") {
    assert(after_exit(0, 7) == 0)?;
    assert(after_exit(3, 2) == 9)?;
}

test("a local the loop writes is not invariant") {
    assert(changes(3) == 60)?;
}

test("for loops") {
    assert(over([1, 2], 4) == 7.0)?;
}
