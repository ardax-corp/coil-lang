// A coroutine handle keeps its position: a `for` loop that breaks leaves the
// rest for a later `resume`, and handles kept in a `Vec` or captured by a
// lambda advance independently.
use tests::assert;

gen fn naturals(int from) {
    let i = from;
    while true {
        yield i;
        i += 1;
    }
}

gen fn three(int base) -> int {
    yield base;
    yield base + 1;
    yield base + 2;
}

test("a lambda capturing a handle advances it") {
    let h = naturals(1);
    let next = fn () use (h) => resume h;
    assert!(next() + next() + next() == 6);
    assert!((resume h) == 4);
}

test("a for loop that breaks leaves the rest for later") {
    let h = naturals(0);
    let seen = 0;
    for v in h {
        if v == 3 {
            break;
        }
        seen += 1;
    }
    assert!(seen == 3);
    assert!((resume h) == 4);
}

test("continue in a for loop over a coroutine") {
    let odd = 0;
    for v in three(4) {
        if v % 2 == 0 {
            continue;
        }
        odd += v;
    }
    assert!(odd == 5);
}

test("handles in a Vec advance independently") {
    let hs = Vec::from([naturals(0), naturals(100)]);
    let _ = resume hs[0];
    let _ = resume hs[0];
    assert!((resume hs[0]) == 2);
    assert!((resume hs[1]) == 100);
}

test("done follows the last yield of a fall-off coroutine") {
    let h = three(0);
    let _ = resume h;
    let _ = resume h;
    let _ = resume h;
    assert!(done(h) == false);
    let _ = resume h;
    assert!(done(h));
}
