// The `return` value is computed before the function's defers run, and a
// `return` from inside nested loops still runs every defer armed so far,
// last first; a `break` or the end of a loop iteration does not run them.
// (A defer inside two nested `for` range loops is a known ICE; findings.)
use tests::assert;

static let log: string = "";

fn note(string s) -> int {
    log = log + s;
    return len(log);
}

fn value_then_defer() -> int {
    defer {
        note("d");
    }
    return note("r");
}

fn exit_from_loops(int stop) -> int {
    defer {
        note("1");
    }
    let i = 0;
    while i < 5 {
        for j in 0..5 {
            if i * 5 + j == stop {
                defer {
                    note("2");
                }
                return i * 10 + j;
            }
        }
        i = i + 1;
    }
    return -1;
}

fn break_does_not_run(int n) -> int {
    defer {
        note("x");
    }
    let i = 0;
    while true {
        if i == n {
            break;
        }
        note(".");
        i = i + 1;
    }
    note("end");
    return i;
}

test("the return value is computed before the defer") {
    log = "";
    assert!(value_then_defer() == 1);
    assert!(log == "rd");
}

test("return from nested loops runs the armed defers") {
    log = "";
    assert!(exit_from_loops(7) == 12);
    assert!(log == "21");
    log = "";
    assert!(exit_from_loops(99) == -1);
    assert!(log == "1");
    assert!(exit_from_loops(0) == 0);
    assert!(exit_from_loops(5) == 10);
    assert!(exit_from_loops(24) == 44);
    assert!(exit_from_loops(25) == -1);
}

test("break and iterations do not run defers") {
    log = "";
    assert!(break_does_not_run(3) == 3);
    assert!(log == "...endx");
}
