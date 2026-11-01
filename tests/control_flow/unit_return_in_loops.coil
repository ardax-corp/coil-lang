// `return;` leaves a unit function from anywhere, including nested loops,
// and the statements after the loop then do not run.
use tests::assert;

static let seen: int = 0;

fn scan(Vec<int> xs, int stop) {
    for x in xs {
        let i = 0;
        while i < x {
            if seen == stop {
                return;
            }
            seen = seen + 1;
            i = i + 1;
        }
    }
    seen = seen + 1000;
}

test("return in a nested loop skips the rest") {
    seen = 0;
    scan([2, 3], 4);
    assert!(seen == 4);
}

test("no return falls through") {
    seen = 0;
    scan([2, 3], 99);
    assert!(seen == 1005);
}
