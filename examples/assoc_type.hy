// Expected output: 42
//
// Phase 6: associated types and projections.
// `type Elem;` in the trait; `type Elem = int;` in the impl.
// Method return uses bare `Elem`, resolved to the class assoc type.
// Open projection `C::Elem` under `C: Collect` is pinned to `int` when
// `take_head` is applied at a ground `Option<int>` call site.

use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};
trait Collect<C> {
    type Elem;
    fn head(C xs) -> Elem;
}

impl Collect<Option<int>> {
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
