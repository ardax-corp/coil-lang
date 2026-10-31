// `task::blocking_with(f, arg)` is generic, so its argument travels boxed;
// the worker thread must get the box back with its tag or the adapter
// unboxes nothing (an empty string, or a null-pointer abort) (#1195).
use tests::assert;
use task::blocking_with;
use thread::ThreadError;

fn size(string name) -> int {
    return name.len();
}

fn greet(string name) -> string {
    return "hi " + name;
}

fn halve(float x) -> float {
    return x / 2.0;
}

fn negate(bool b) -> bool {
    return !b;
}

fn total(Vec<int> xs) -> int {
    let sum = 0;
    for x in xs {
        sum += x;
    }
    return sum;
}

fn or_default<T>(Result<T, ThreadError> r, T fallback) -> T {
    return match r {
        Result::Ok(v) => v,
        Result::Err(_) => fallback,
    };
}

test("blocking_with passes a string argument") {
    assert!(or_default(blocking_with(size, "ada"), -1) == 3);
}

test("blocking_with returns a string built from a string argument") {
    assert!(or_default(blocking_with(greet, "ada"), "") == "hi ada");
}

test("blocking_with passes float, bool and Vec arguments") {
    assert!(or_default(blocking_with(halve, 5.0), 0.0) == 2.5);
    assert!(or_default(blocking_with(negate, false), false));
    assert!(or_default(blocking_with(total, [1, 2, 3, 4]), -1) == 10);
}
