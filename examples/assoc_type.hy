// Associated types: `type Elem;` in a trait, `type Elem = int;` in an
// instance. A method returns the bare `Elem`; the projection `C::Elem` under
// `C: Collect` resolves to `int` at a ground call site.
//
// @output 42

use io::sync::print;
use string::format;

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
    let _ = print(format("%i", take_head(Option::Some(42))));
}
