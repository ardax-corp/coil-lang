// A loop inside a `defer` body keeps its own `break` and `continue`, and a
// lambda inside it its own `return` (coil-lang#2134).
use tests::assert;
use task::{scope, Scope};

static let LOG: string = "";

fn say(string s) {
    LOG = LOG + s;
}

fn with_while() -> int {
    defer {
        let i = 0;
        while true {
            i = i + 1;
            if i > 2 {
                break;
            }
        }
        say("w" + i.show());
    }
    return 1;
}

fn with_for() -> int {
    defer {
        for i in 0..5 {
            if i % 2 == 0 {
                continue;
            }
            if i > 3 {
                break;
            }
            say("f" + i.show());
        }
    }
    say("body");
    return 2;
}

fn with_lambda() -> int {
    defer {
        let pick = fn (int x) {
            if x > 1 {
                return x;
            }
            return 0;
        };
        say("l" + pick(5).show());
    }
    return 3;
}

test("a loop in a defer body breaks and continues inside the body") {
    LOG = "";
    assert!(with_while() == 1);
    assert!(LOG == "w3", LOG);
    LOG = "";
    assert!(with_for() == 2);
    assert!(LOG == "bodyf1f3", LOG);
}

test("a lambda in a defer body returns from the lambda") {
    LOG = "";
    assert!(with_lambda() == 3);
    assert!(LOG == "l5", LOG);
}

fn maybe(bool some) -> Option<int> {
    if some {
        return Option::Some(7);
    }
    return Option::None;
}

fn with_match(bool some) -> int {
    defer use (some) {
        match maybe(some) {
            Option::Some(v) => say("m" + v.show()),
            Option::None => say("none"),
        }
    }
    return 4;
}

test("a match in a defer body binds its payload") {
    LOG = "";
    assert!(with_match(true) == 4);
    assert!(with_match(false) == 4);
    assert!(LOG == "m7none", LOG);
}

fn unwinds() -> int {
    defer {
        for i in 0..4 {
            if i == 3 {
                break;
            }
            say("u" + i.show());
        }
    }
    panic "unwind";
}

test("a loop in a defer body runs while a panic unwinds") {
    LOG = "";
    let _ = scope(fn (Scope s) {
        let _ = s.spawn(fn () => unwinds());
        0
    });
    assert!(LOG == "u0u1u2", LOG);
}
