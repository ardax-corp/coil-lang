// Short counted loops unroll into copies of their body; the copies compute
// what the loop did, and the counter ends where the loop left it.
fn counted(int x) -> int {
    let s = x;
    let i = 0;
    while i < 3 {
        s = s * x + i;
        i = i + 1;
    }
    return s * 10 + i;
}

fn bound_local([int] a) -> int {
    let n = 4;
    let s = 0;
    let j = 1;
    while n >= j {
        let t = a[j - 1] * j;
        s = s + t;
        j = j + 1;
    }
    return s * 10 + j;
}

fn nested(int x) -> int {
    let s = 0;
    let k = 0;
    while k < x {
        let i = 0;
        while i < 2 {
            s = s + k * i + 1;
            i = i + 1;
        }
        k = k + 1;
    }
    return s;
}

fn stores([int] a) -> [int] {
    let i = 0;
    while i <= 2 {
        a[i] = a[i] + i;
        i = i + 1;
    }
    return a;
}

test("a counted loop runs every trip") {
    assert(counted(2) == 203)?;
    assert(counted(-1) == 23)?;
}

test("a literal bound local and a body let") {
    assert(bound_local([1, 2, 3, 4]) == 305)?;
}

test("an inner loop unrolls inside its outer loop") {
    assert(nested(3) == 9)?;
}

test("stores land in every slot") {
    let a = stores([5, 5, 5, 5]);
    assert(a[0] == 5 && a[1] == 6 && a[2] == 7 && a[3] == 5)?;
}
