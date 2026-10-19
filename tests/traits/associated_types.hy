// A trait's associated type is defined by each impl, and a method that
// returns the bare `Elem` returns that impl's type.
use tests::assert;

trait Collect<C> {
    type Elem;
    fn head(C xs) -> Elem {}
    fn count(C xs) -> int {}
}

class Words {
    pub items: Vec<string>,
}

impl Collect for Option<int> {
    type Elem = int;

    pub fn head(Option<int> xs) -> int {
        return xs ?? 0;
    }

    pub fn count(Option<int> xs) -> int {
        return match xs {
            Option::Some(_) => 1,
            Option::None => 0,
        };
    }
}

impl Collect for Words {
    type Elem = string;

    pub fn head(Words w) -> string {
        if w.items.len() == 0 {
            return "";
        }
        return w.items[0];
    }

    pub fn count(Words w) -> int {
        return w.items.len();
    }
}

test("each instance defines the associated type") {
    let n: int = head(Option::Some(42));
    assert!(n == 42);
    let w = new Words(Vec::from(["a", "b"]));
    let s: string = head(w);
    assert!(s == "a");
    assert!(count(w) == 2);
}
