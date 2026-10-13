// Function calls, recursion, early return, multiple params.
use tests::assert;

fn add(int a, int b) -> int {
    return a + b;
}

fn fib(int n) -> int {
    if n <= 2 {
        return 1;
    }
    return fib(n - 1) + fib(n - 2);
}

fn early(int n, int is_neg) -> int {
    if is_neg == 1 {
        return 0 - 1;
    }
    return n * 2;
}

/// Body wraps another call, so the callee is a candidate for tiny-call
/// inlining but must be refused (its locals have no caller temp).
fn wraps_call(int n) -> int {
    return 1 + add(n, 0);
}

fn wraps_call_sub(int n) -> int {
    return 10 - add(n, 0);
}

fn multi(int a, int b, int c) -> int {
    return a + b * c;
}

test("basic call") {
    assert!(add(2, 3) == 5);
}

test("recursion fib") {
    assert!(fib(1) == 1);
    assert!(fib(2) == 1);
    assert!(fib(6) == 8);
    assert!(fib(10) == 55);
}

test("early return") {
    assert!(early(4, 1) == 0 - 1);
    assert!(early(4, 0) == 8);
}

test("multi arg precedence in callee") {
    assert!(multi(1, 2, 3) == 7);
}

test("nested calls") {
    assert!(add(add(1, 2), add(3, 4)) == 10);
}

// A refused inline attempt must leave no bytecode behind: leaked arg prep or a
// partially copied body used to run *and* be followed by the real CALL, storing
// into caller slots and clobbering an already-computed operand.
test("call whose body wraps a call") {
    assert!(wraps_call(5) == 6);
    assert!(wraps_call_sub(3) == 7);
}

test("two such calls in one expression keep both operands") {
    assert!(wraps_call(5) + wraps_call_sub(3) == 13);
    assert!(wraps_call(5) + wraps_call(3) == 10);
    let x = wraps_call(5);
    let y = wraps_call_sub(3);
    assert!(x == 6);
    assert!(y == 7);
}
