// B3: two-slot CALL + match must stay unboxed and checksum.

fn lookup(int i, int n) -> Option<int> {
    if i < 0 || i >= n {
        return Option::None;
    }
    return Option::Some(i * 2);
}

fn checked_div(int a, int b) -> Result<int, int> {
    if b == 0 {
        return Result::Err(-1);
    }
    return Result::Ok(a / b);
}

fn hot_opt(int n, int iters) -> int {
    let acc = 0;
    let i = 0;
    while i < iters {
        acc = acc + match lookup(i % 10, n) {
            Option::Some(x) => x,
            Option::None => 0,
        };
        i = i + 1;
    }
    return acc;
}

fn hot_res(int iters) -> int {
    let acc = 0;
    let i = 0;
    while i < iters {
        acc = acc + match checked_div((i % 10) + 1, i % 5) {
            Result::Ok(q) => q,
            Result::Err(e) => e,
        };
        i = i + 1;
    }
    return acc;
}

test("option match+call checksum") {
    assert(hot_opt(7, 40) == 168)?;
}

test("result match+call checksum") {
    // period-10 of checked_div((i%10)+1, i%5): 19 * 4 periods = 76
    assert(hot_res(40) == 76)?;
}
