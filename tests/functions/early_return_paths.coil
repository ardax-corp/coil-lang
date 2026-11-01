// `return` leaves the function from anywhere: inside nested loops, from a
// match arm block in a `let` initializer, and as a bare `return;` in a
// unit function.
use tests::assert;

class Log {
    pub hits: int,
}

fn find(int n) -> int {
    let i = 0;
    while i < 10 {
        for j in 0..10 {
            if i * 10 + j == n {
                return i * 100 + j;
            }
        }
        i = i + 1;
    }
    return -1;
}

fn label(int n) -> string {
    let s = match n {
        0 => {
            return "zero";
        },
        default => "n",
    };
    return s + "!";
}

fn touch(Log log, int n) {
    if n < 0 {
        return;
    }
    log.hits = log.hits + 1;
}

test("return from nested loops") {
    assert!(find(0) == 0);
    assert!(find(23) == 203);
    assert!(find(99) == 909);
    assert!(find(100) == -1);
}

test("return from a match arm inside a let") {
    assert!(label(0) == "zero");
    assert!(label(4) == "n!");
}

test("bare return in a unit function") {
    let log = new Log(0);
    touch(log, 1);
    touch(log, -1);
    touch(log, 2);
    assert!(log.hits == 2);
    touch(log, 0);
    assert!(log.hits == 3);
}
