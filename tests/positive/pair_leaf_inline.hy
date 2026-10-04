// Leaf functions returning a two-word Option/Result are inlined at direct
// call sites; every way the caller consumes the pair must still see it.

fn lookup(int i, int n) -> Option<int> {
    if i < 0 || i >= n {
        return Option::None;
    }
    return Option::Some(i * 2);
}

fn checked_half(int n) -> Result<int, string> {
    if n % 2 == 1 {
        return Result::Err("odd");
    }
    return Result::Ok(n / 2);
}

fn name_of(int i) -> Option<string> {
    if i == 0 {
        return Option::Some("zero");
    }
    return Option::None;
}

fn sum_lookups(int n) -> int {
    let acc = 0;
    let i = -2;
    while i < n + 2 {
        acc = acc + match lookup(i, n) {
            Option::Some(x) => x,
            Option::None => 100,
        };
        i = i + 1;
    }
    return acc;
}

fn halve_twice(int n) -> Result<int, string> {
    let h = checked_half(n)?;
    return checked_half(h)?;
}

fn keep(Option<int> o) -> Option<int> {
    return o;
}

test("match on an inlined pair") {
    // -2, -1, 7, 8 miss; 0..6 hit with 0+2+..+12 = 42.
    assert(sum_lookups(7) == 442)?;
}

test("inlined pair bound, passed and compared") {
    let o = lookup(3, 7);
    assert(o == Option::Some(6))?;
    assert(keep(lookup(9, 7)) == Option::None)?;
    assert((lookup(1, 7) ?? -1) + (lookup(-1, 7) ?? -1) == 1)?;
    assert((lookup(1, 7) ?? -1) - (lookup(3, 7) ?? -1) == -4)?;
}

test("inlined Result with ? propagation") {
    assert(halve_twice(8) == Result::Ok(2))?;
    assert(halve_twice(6) == Result::Err("odd"))?;
    assert(halve_twice(3) == Result::Err("odd"))?;
}

test("inlined pair with a heap payload") {
    assert((name_of(0) ?? "none") == "zero")?;
    assert((name_of(1) ?? "none") == "none")?;
}
