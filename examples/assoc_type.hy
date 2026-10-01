// Associated types: `type Elem;` in a trait, `type Elem = int;` in an
// instance. A method returns the bare `Elem`; the projection `C::Elem` under
// `C: Collect` resolves to `int` at a ground call site.
//
// Output: 42

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

trait Collect<C> {
    type Elem;
    fn head(C xs) -> Elem {}
}

impl Collect for Option<int> {
    type Elem = int;
    pub fn head(Option<int> xs) -> int {
        return match xs {
            Option::Some(v) => v,
            Option::None => 0,
        };
    }
}

fn take_head<C: Collect>(C xs) -> C::Elem {
    return head(xs);
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", take_head(Option::Some(42)))));
}
