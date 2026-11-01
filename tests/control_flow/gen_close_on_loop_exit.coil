// A `for` loop over a generator that it leaves early (`break`, `return`,
// `?`) closes the generator: the `defer`s armed at its `yield` run, last
// first, before the loop's code goes on. A loop that runs the generator
// to its end closes nothing more, and neither does one over a generator a
// local holds: a later `resume` may go on with it.
use tests::assert;
use task::{scope, Scope, TaskError};

static let LOG: string = "";

fn say(string s) {
    LOG = LOG + s;
}

gen fn nums(int n) {
    defer {
        say("[outer]");
    }
    let i = 0;
    while i < n {
        {
            defer use (i) {
                say("[" + i.show() + "]");
            }
            yield i;
        }

        i = i + 1;
    }
    return 0;
}

fn breaks(int stop) -> int {
    let sum = 0;
    for v in nums(5) {
        if v == stop {
            break;
        }
        sum = sum + v;
    }
    say("|");
    return sum;
}

test("a break closes the generator at its yield") {
    LOG = "";
    assert!(breaks(2) == 1);
    assert!(LOG == "[0][1][2][outer]|", LOG);
}

fn first(int n) -> int {
    for v in nums(n) {
        return v + 10;
    }
    return -1;
}

test("a return from the body closes the generator") {
    LOG = "";
    assert!(first(3) == 10);
    say("|");
    assert!(LOG == "[0][outer]|", LOG);
}

test("running the generator to its end closes nothing more") {
    LOG = "";
    let sum = 0;
    for v in nums(2) {
        sum = sum + v;
    }
    assert!(sum == 1);
    assert!(LOG == "[0][1][outer]", LOG);
}

fn skips() -> int {
    let n = 0;
    for v in nums(3) {
        if v == 1 {
            continue;
        }
        n = n + 1;
    }
    return n;
}

test("a continue does not close the generator") {
    LOG = "";
    assert!(skips() == 2);
    assert!(LOG == "[0][1][2][outer]", LOG);
}

test("a loop over a held generator leaves it open") {
    LOG = "";
    let h = nums(3);
    for v in h {
        break;
    }
    assert!(LOG == "", LOG);
    assert!((resume h) == 1);
    assert!(LOG == "[0]", LOG);
}

fn pick(Option<int> o) -> Option<int> {
    for v in nums(4) {
        let k = o?;
        if v == k {
            return Some(v);
        }
    }
    return None;
}

test("a `?` leaving the body closes the generator") {
    LOG = "";
    assert!(pick(None) == None);
    assert!(LOG == "[0][outer]", LOG);
    LOG = "";
    assert!(pick(Some(1)) == Some(1));
    assert!(LOG == "[0][1][outer]", LOG);
}

fn nested() -> int {
    let n = 0;
    for a in nums(2) {
        for b in nums(3) {
            if b == 1 {
                break;
            }
            n = n + 1;
        }
        say("/");
    }
    return n;
}

test("an inner loop's break closes only its own generator") {
    LOG = "";
    assert!(nested() == 2);
    assert!(LOG == "[0][1][outer]/[0][0][1][outer]/[1][outer]", LOG);
}

gen fn loud() {
    defer {
        say("[loud]");
        panic "in close";
    }
    yield 1;
    yield 2;
    return 0;
}

fn closes_loud() -> int {
    for v in loud() {
        break;
    }
    say("unreached");
    return 0;
}

fn failed(Result<int, TaskError> r) -> bool {
    return match r {
        Result::Err(TaskError::Panicked(m)) => m == "in close",
        default => false,
    };
}

test("a panic in a defer the close runs goes on to the loop's caller") {
    LOG = "";
    assert!(
        failed(
            scope(fn (Scope s) {
                let _ = s.spawn(fn () => closes_loud());
                0
            }),
        ),
    );
    assert!(LOG == "[loud]", LOG);
}
