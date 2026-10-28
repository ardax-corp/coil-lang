// A `panic` aborts the task it runs in (here a child task), carrying its
// message; as it unwinds, every `defer` already reached in the functions it
// leaves still runs, innermost first. Code after the panic does not run.
use tests::assert;
use task::{scope, Scope, TaskError};

class Trace {
    pub text: string,
}

fn inner(Trace t, int n) -> int {
    defer use (t) {
        t.text = t.text + "i";
    }
    if n > 0 {
        panic "inner " + n.show();
    }
    return n;
}

fn outer(Trace t, int n) -> int {
    defer use (t) {
        t.text = t.text + "o";
    }
    let v = inner(t, n);
    t.text = t.text + "after";
    return v;
}

fn message(Result<int, TaskError> r) -> string {
    return match r {
        Result::Ok(_) => "ok",
        Result::Err(TaskError::Panicked(m)) => m,
        Result::Err(_) => "other",
    };
}

fn outcome(unit -> int body) -> string {
    return message(scope(fn (Scope s) use (body) {
        let _ = s.spawn(body);
        0
    }));
}

test("defers run while a panic unwinds, code after it does not") {
    let t = new Trace("");
    assert!(outcome(fn () use (mut t) => outer(t, 3)) == "inner 3");
    assert!(t.text == "io");
    assert!(outcome(fn () use (mut t) => outer(t, 1)) == "inner 1");
    assert!(t.text == "ioio");
}

test("without a panic the defers run in the normal order") {
    let t = new Trace("");
    assert!(outcome(fn () use (mut t) => outer(t, 0)) == "ok");
    assert!(t.text == "iaftero");
}
