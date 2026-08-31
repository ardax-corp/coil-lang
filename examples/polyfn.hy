// Expected output: 424.0424242

use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};
trait Describable<T> {
    fn describe_val(T x) -> int;
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
    write_all(stdout(), to_bytes(format("%i", f(42))));
    write_all(stdout(), to_bytes(format("%f", f(4.0))));

    let constrained = show;
    write_all(stdout(), to_bytes(format("%i", constrained(41))));
    write_all(stdout(), to_bytes(format("%i", apply_id(id, 42))));

    let captured = capture_show(0);
    write_all(stdout(), to_bytes(format("%i", captured(41))));
}
