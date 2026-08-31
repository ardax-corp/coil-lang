// while / for / break / continue.
test("while counts up") {
    let i = 0;
    let sum = 0;
    while i < 5 {
        sum = sum + i;
        i = i + 1;
    }
    assert(sum == 10)?;
    assert(i == 5)?;
}

test("while never enters") {
    let x = 0;
    while false {
        x = 1;
    }
    assert(x == 0)?;
}

test("while sum") {
    let sum = 0;
    let i = 0;
    while i < 5 {
        sum = sum + i;
        i = i + 1;
    }
    assert(sum == 10)?;
}

test("while continue skips") {
    let sum = 0;
    let i = 0;
    while i < 6 {
        if i == 3 {
            i = i + 1;
            continue;
        }
        sum = sum + i;
        i = i + 1;
    }
    assert(sum == 12)?; // 0+1+2+4+5
}

test("while break exits early") {
    let sum = 0;
    let i = 0;
    while i < 100 {
        if i == 4 {
            break;
        }
        sum = sum + i;
        i = i + 1;
    }
    assert(sum == 6)?; // 0+1+2+3
}

test("while continue and break together") {
    let sum = 0;
    let i = 0;
    while i < 10 {
        if i == 3 {
            i = i + 1;
            continue;
        }
        if i == 7 {
            break;
        }
        sum = sum + i;
        i = i + 1;
    }
    assert(sum == 18)?; // 0+1+2+4+5+6
}

test("postfix increment in loop") {
    let y = 0;
    let n = 0;
    while n < 3 {
        y = y + 1;
        n = n + 1;
    }
    assert(y == 3)?;
}

test("nested loops") {
    let total = 0;
    let i = 0;
    while i < 3 {
        let j = 0;
        while j < 3 {
            total = total + 1;
            j = j + 1;
        }
        i = i + 1;
    }
    assert(total == 9)?;
}
