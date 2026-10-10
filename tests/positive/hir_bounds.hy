// Counted loops whose index sites are proven in bounds still compute the
// same sums, and sites the proof must leave checked still read the right
// element.
fn sum_while([int] a) -> int {
    let s = 0;
    let i = 0;
    while i < len(a) {
        s = s + a[i];
        a[i] = 0;
        i = i + 1;
    }
    return s;
}

fn sum_range([int] a) -> int {
    let s = 0;
    for i in 0..len(a) {
        s = s + a[i] * 2;
    }
    return s;
}

fn after_bump([int] a) -> int {
    let s = 0;
    let i = 0;
    while i < len(a) {
        i = i + 1;
        if i < len(a) {
            s = s + a[i];
        }
    }
    return s;
}

fn filled(int n) -> int {
    let v: Vec<int> = Vec::with_capacity(n);
    let i = 0;
    while i < n {
        v.push(i);
        i = i + 1;
    }
    let s = 0;
    let j = 0;
    while j < n {
        s = s + v[j];
        j = j + 1;
    }
    return s;
}

fn bound_let([int] a) -> int {
    let n = len(a);
    let s = 0;
    let i = 0;
    while i < n {
        s = s + a[i];
        i = i + 2;
    }
    return s;
}

test("a counted while sums and clears") {
    let a = [1, 2, 3, 4];
    assert(sum_while(a) == 10)?;
    assert(a[0] + a[3] == 0)?;
}

test("a range loop over the length") {
    assert(sum_range([1, 2, 3]) == 12)?;
}

test("a read after the bump") {
    assert(after_bump([5, 6, 7]) == 13)?;
}

test("a filled vec") {
    assert(filled(5) == 10)?;
    assert(filled(0) == 0)?;
}

test("a bound from a len let") {
    assert(bound_let([1, 2, 3, 4, 5]) == 9)?;
}
