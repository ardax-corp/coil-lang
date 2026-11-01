// A `defer` runs when its enclosing block ends: falling off the end, a
// `return`, a `break` or `continue` out of it, or a panic. Defers of one
// block run last first, and an inner block's run before the outer's.
use tests::assert;
use task::{scope, Scope, TaskError};

static let LOG: string = "";

fn say(string s) {
    LOG = LOG + s;
}

fn nested() {
    say("a");
    {
        defer {
            say("[inner]");
        }
        say("b");
    }
    say("c");
}

test("a nested block's defer runs at the block's end") {
    LOG = "";
    nested();
    assert!(LOG == "ab[inner]c");
}

fn per_iteration() {
    for i in 0..3 {
        defer use (i) {
            say("d" + i.show());
        }
        say("i" + i.show());
    }
    say(".");
}

test("a loop body's defer runs every iteration") {
    LOG = "";
    per_iteration();
    assert!(LOG == "i0d0i1d1i2d2.");
}

fn leaves(int stop) {
    let i = 0;
    while i < 5 {
        defer use (i) {
            say("d" + i.show());
        }
        i = i + 1;
        if i == 2 {
            continue;
        }
        if i == stop {
            break;
        }
        say("i" + i.show());
    }
    say(".");
}

test("break and continue run the loop body's defers") {
    LOG = "";
    leaves(3);
    assert!(LOG == "i1d0d1d2.");
}

fn branch(bool on) -> int {
    if on {
        defer {
            say("[then]");
        }
        say("t");
    }
    say("x");
    return 1;
}

test("an `if` arm's defer runs when the arm ends") {
    LOG = "";
    assert!(branch(true) == 1);
    assert!(branch(false) == 1);
    assert!(LOG == "t[then]xx");
}

fn order() -> int {
    defer {
        say("[outer]");
    }
    {
        defer {
            say("[1]");
        }
        defer {
            say("[2]");
        }
        if LOG == "" {
            return 7;
        }
    }
    return 0;
}

test("a return runs inner then outer defers, last first") {
    LOG = "";
    assert!(order() == 7);
    assert!(LOG == "[2][1][outer]");
}

fn valued(bool on) -> int {
    let v = if on {
        defer {
            say("[d]");
        }
        say("v");
        5
    } else {
        0
    };
    say("after");
    return v;
}

test("a block's value is made before its defer runs") {
    LOG = "";
    assert!(valued(true) == 5);
    assert!(LOG == "v[d]after");
}

fn unwinds() -> int {
    for i in 0..3 {
        defer use (i) {
            say("d" + i.show());
        }
        if i == 1 {
            panic "boom";
        }
    }
    return 0;
}

fn failed(Result<int, TaskError> r) -> bool {
    return match r {
        Result::Err(TaskError::Panicked(_)) => true,
        default => false,
    };
}

test("a panic runs the armed defers of the blocks it leaves") {
    LOG = "";
    assert!(
        failed(
            scope(fn (Scope s) {
                let _ = s.spawn(fn () => unwinds());
                0
            }),
        ),
    );
    assert!(LOG == "d0d1");
}
