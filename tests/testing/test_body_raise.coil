// A test body is in result mode, so it may `raise` directly (the shape
// `assert!` expands to).
test("ok") {
    if !true {
        raise "assertion failed";
    }
}

test("second") {
    if !(1 == 1) {
        raise "assertion failed";
    }
}
