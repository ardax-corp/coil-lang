// A closure a call returns, called in place without a `let` (#1533):
// a named function's, a generic function's at a ground type, and a
// returned closure's own returned closure.
use tests::assert;
use string::format;

fn adder(int k) -> int -> int {
    return fn (int y) use (k) => y + k;
}

fn curry(int a) -> int -> int -> int {
    return fn (int b) use (a) => fn (int c) use (a, b) => a * 100 + b * 10 + c;
}

fn maker<T>(T x) -> unit -> T {
    return fn () use (x) => x;
}

fn greeter(string hello) -> string -> string {
    return fn (string who) use (hello) => hello + " " + who;
}

test("call a returned closure in place") {
    assert!(adder(3)(4) == 7);
    assert!(adder(adder(1)(1))(5) == 7);
    assert!(greeter("hi")("coil") == "hi coil");
}

test("call a returned closure's returned closure in place") {
    assert!(curry(1)(2)(3) == 123);
}

test("call a generic function's returned closure in place") {
    assert!(maker(10)() == 10);
    assert!(maker("ten")() == "ten");
    assert!(format("%i", maker(4)()) == "4");
    let (a, b) = maker((1, 2))();
    assert!(a + b == 3);
}
