// Helper for tests/functions/over_application_imported.hy: curried
// functions imported by their bare names.

fn adder(int a) -> int -> int {
    return fn (int b) use (a) => a * 10 + b;
}

fn pair_with<T: Show>(T a) -> T -> string {
    return fn (T b) use (a) => string::format("%v,%v", a, b);
}
