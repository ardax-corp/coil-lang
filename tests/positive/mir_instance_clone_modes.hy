// An instance clone of an inherent generic method is checked in its
// method's result mode, as the method itself is; and a default static
// trait method's call of a sibling named only by its result type finds
// the instance from the call's type. Both clones lower through HIR.
use tests::assert;

enum Fail {
    Low,
    Full,
}

class Slots<T> {
    pub items: Vec<Option<T>>,
}

impl Slots<T> {
    pub fn put(int at, T value) -> Result<unit, Fail> {
        if at < 0 {
            return Result::Err(Fail::Low);
        }
        if at >= self.items.len() {
            return Result::Err(Fail::Full);
        }
        self.items[at] = Option::Some(value);
        return Result::Ok(());
    }
}

trait Make<T> {
    static fn from_pair(int a, int b) -> T {}

    static fn from_one(int a) -> T {
        return from_pair(a, a);
    }
}

impl Make for int {
    pub static fn from_pair(int a, int b) -> int {
        return a + b;
    }
}

impl Make for string {
    pub static fn from_pair(int a, int b) -> string {
        return "pair";
    }
}

fn code(Result<unit, Fail> r) -> int {
    return match r {
        Result::Ok(_) => 0,
        Result::Err(Fail::Low) => 1,
        Result::Err(Fail::Full) => 2,
    };
}

test("an inherent generic method's clone keeps its result mode") {
    let v: Vec<Option<int>> = Vec::new();
    v.push(Option::None);
    let s = new Slots(v);
    assert!(code(s.put(0, 5)) == 0);
    assert!(code(s.put(-1, 5)) == 1);
    assert!(code(s.put(3, 5)) == 2);
}

test("a default static method calls a result-typed sibling") {
    let n: int = int::from_one(4);
    assert!(n == 8);
    let w: string = string::from_one(1);
    assert!(w == "pair");
}
