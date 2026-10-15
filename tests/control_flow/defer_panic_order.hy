// A panic runs the `defer`s of every frame it leaves, innermost first, with
// captures read at the panic; a `defer` whose statement never ran does not
// run; a panic inside a `defer` stops that function's cleanup only.
use tests::assert;
use task::{scope, Scope, TaskError};

static let LOG: string = "";

fn say(string s) {
    LOG = LOG + s;
}

fn message(Result<int, TaskError> r) -> string {
    return match r {
        Result::Ok(_) => "ok",
        Result::Err(TaskError::Panicked(m)) => m,
        Result::Err(_) => "other",
    };
}

// Run `body` as a child task; its panic message, or "ok".
fn outcome(unit -> int body) -> string {
    return message(scope(fn (Scope s) use (body) {
        let _ = s.spawn(body);
        0
    }));
}

fn inner(int k) -> int {
    defer use (k) {
        say("[inner " + k.show() + "]");
    }
    if k > 2 {
        panic "boom";
    }
    return k;
}

fn outer() -> int {
    let n = 1;
    defer use (n) {
        say("[outer " + n.show() + "]");
    }
    n = 5;
    say("a");
    let r = inner(n);
    say("unreached");
    return r;
}

fn entry() -> int {
    defer {
        say("[main]");
    }
    outer();
    return 0;
}

fn f(bool b, bool fail) -> int {
    defer {
        say("A");
    }
    if b {
        defer {
            say("B");
        }
        say("x");
    }
    if fail {
        panic "stop";
    }
    say("y");
    return 1;
}

fn inner_twice() -> int {
    defer {
        say("[inner]");
        panic "second";
    }
    panic "first";
}

fn entry_twice() -> int {
    defer {
        say("[main]");
    }
    inner_twice();
    return 0;
}

test("a panic runs the defers of the frames it leaves") {
    LOG = "";
    assert!(outcome(fn () => entry()) == "boom");
    assert!(LOG == "a[inner 5][outer 5][main]", LOG);
}

test("a defer runs only once its statement ran") {
    LOG = "";
    f(false, false);
    say("|");
    f(true, false);
    say("|");
    assert!(outcome(fn () => f(false, true)) == "stop");
    assert!(LOG == "yA|xyBA|A", LOG);
}

test("a panic inside a defer keeps unwinding the callers") {
    LOG = "";
    let m = outcome(fn () => entry_twice());
    assert!(LOG == "[inner][main]", LOG);
    // The panic raised last is the one the task reports.
    assert!(m == "second", m);
}
