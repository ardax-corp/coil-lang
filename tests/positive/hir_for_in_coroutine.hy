// `for x in coroutine`: resume into `x`, stop once the handle is done;
// the completion value is never bound.
async fn upto(int n) {
    let i = 0;
    while i < n {
        yield i;
        i = i + 1;
    }
    return 100;
}

fn total(int n) -> int {
    let acc = 0;
    for x in upto(n) {
        acc = acc + x;
    }
    return acc;
}

fn odd_until(int n, int stop) -> int {
    let acc = 0;
    for x in upto(n) {
        if x % 2 == 0 {
            continue;
        }
        if x > stop {
            break;
        }
        acc = acc + x;
    }
    return acc;
}

test("sum of yields skips the completion value") {
    assert(total(5) == 10)?;
    assert(total(0) == 0)?;
}

test("continue and break") {
    assert(odd_until(10, 5) == 9)?;
    assert(odd_until(10, 100) == 25)?;
}
