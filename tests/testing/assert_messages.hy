// `assert!` raises `Err` from the enclosing result-mode function: with a
// message it raises that string (computed at run time too); without one it
// raises "Assertion: '<condition as written>' failed". The condition is
// evaluated once, and nothing after a failing assert runs.
use tests::assert;

static let evals: int = 0;

static let after: int = 0;

fn tick(bool v) -> bool {
    evals += 1;
    return v;
}

fn check_default(int n) -> Result<(), string> {
    assert!(n * 2 == 10);
    return Result::Ok(());
}

fn check_strings(string s) -> Result<(), string> {
    assert!(s == "a b");
    return Result::Ok(());
}

fn check_built_message(int n) -> Result<(), string> {
    assert!(n < 3, "too big: " + n.show());
    return Result::Ok(());
}

fn check_counted(bool v) -> Result<(), string> {
    assert!(tick(v), "tick");
    after += 1;
    return Result::Ok(());
}

fn check_two(int a, int b) -> Result<int, string> {
    assert!(a > 0, "a");
    assert!(b > 0, "b");
    return a + b;
}

fn err_of<T>(Result<T, string> r) -> string {
    return match r {
        Result::Ok(_) => "ok",
        Result::Err(e) => e,
    };
}

test("the default message quotes the condition as written") {
    assert!(err_of(check_default(5)) == "ok");
    assert!(err_of(check_default(4)) == "Assertion: 'n * 2 == 10' failed");
}

test("a condition with a string literal keeps its quotes") {
    assert!(err_of(check_strings("a b")) == "ok");
    assert!(err_of(check_strings("x")) == "Assertion: 's == \"a b\"' failed");
}

test("a message computed at run time") {
    assert!(err_of(check_built_message(7)) == "too big: 7");
}

test("the condition runs once and a failure stops the function") {
    evals = 0;
    after = 0;
    assert!(err_of(check_counted(true)) == "ok");
    assert!(evals == 1 && after == 1);
    assert!(err_of(check_counted(false)) == "tick");
    assert!(evals == 2 && after == 1);
}

test("the first failing assert wins") {
    assert!(check_two(1, 2) == Result::Ok(3));
    assert!(err_of(check_two(0, 0)) == "a");
    assert!(err_of(check_two(1, 0)) == "b");
}
