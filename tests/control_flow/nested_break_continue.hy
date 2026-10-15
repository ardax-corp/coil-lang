// `break` and `continue` act on the innermost enclosing loop only (Coil has
// no labels), across every mix of `while` and `for … in`; the outer loop
// keeps going after an inner `break`.
use tests::assert;

test("inner break leaves only the inner while") {
    let pairs = 0;
    let i = 0;
    while i < 4 {
        let j = 0;
        while true {
            if j == i {
                break;
            }
            pairs = pairs + 1;
            j = j + 1;
        }
        i = i + 1;
    }
    assert!(pairs == 0 + 1 + 2 + 3);
    assert!(i == 4);
}

test("inner continue skips only the inner iteration") {
    let s = 0;
    for i in 0..3 {
        for j in 0..4 {
            if j % 2 == 1 {
                continue;
            }
            s = s + 10 * i + j;
        }
    }
    assert!(s == (0 + 2) + (10 + 12) + (20 + 22));
}

test("for inside while and while inside for") {
    let hits = 0;
    let n = 0;
    while n < 3 {
        n = n + 1;
        for x in [1, 2, 3, 4] {
            if x > n {
                break;
            }
            hits = hits + 1;
        }
    }
    assert!(hits == 1 + 2 + 3);
    let count = 0;
    for x in 0..5 {
        let k = 0;
        while k < 10 {
            k = k + 1;
            if k <= x {
                continue;
            }
            count = count + 1;
            break;
        }
    }
    assert!(count == 5);
}

test("outer continue after an inner loop") {
    let s = 0;
    for i in 0..4 {
        let inner = 0;
        for j in 0..i {
            inner = inner + j;
        }
        if inner == 1 {
            continue;
        }
        s = s + inner;
    }
    assert!(s == 0 + 0 + 3);
}

test("three levels deep") {
    let total = 0;
    for a in 0..3 {
        for b in 0..3 {
            for c in 0..3 {
                if c == 2 {
                    break;
                }
                if b == 1 {
                    continue;
                }
                total = total + 1;
            }
        }
    }
    assert!(total == 3 * 2 * 2);
}
