// A trait method called from a monomorphized generic finds the instance for
// a constructed argument (`Option::Some(42)` is `Option<int>`).
trait Collect<C> {
    type Elem;
    fn head(C xs) -> Elem;
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

test("trait call on a constructed arg") {
    assert(take_head(Option::Some(42)) == 42)?;
}
