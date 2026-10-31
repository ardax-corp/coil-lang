// Polymorphic function values: `forall T. T -> T` parameters, and a function
// returned with its trait evidence captured.
//
// @output 424.0424242

use io::sync::print;
use string::format;

trait Describable<T> {
    fn describe_val(T x) -> int {}
}

impl Describable for int {
    pub fn describe_val(int x) -> int {
        return x + 1;
    }
}

fn id<T>(T x) -> T {
    return x;
}

fn show<T: Describable>(T x) -> int {
    return describe_val(x);
}

fn apply_id(forall T. T -> T f, int x) -> int {
    return f(x);
}

// Capture Describable evidence into a PolyFn and return it. After this
// frame returns, CallIndirect must still see the captured dictionary
// (app_dict_arity=0 at the use site).
fn capture_show<T: Describable>(T _witness) {
    return show;
}

fn main() {
    let f = id;
    let _ = print(format("%i", f(42)));
    let _ = print(format("%f", f(4.0)));

    let constrained = show;
    let _ = print(format("%i", constrained(41)));
    let _ = print(format("%i", apply_id(id, 42)));

    let captured = capture_show(0);
    let _ = print(format("%i", captured(41)));
}
