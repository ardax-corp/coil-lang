// Calls to functions and methods declared later in the file lower from HIR
// (arity from the signature); ones returning an enum stay on the AST.

fn is_even(int n) -> bool {
    if n == 0 {
        return true;
    }
    return is_odd(n - 1);
}

fn is_odd(int n) -> bool {
    if n == 0 {
        return false;
    }
    return is_even(n - 1);
}

fn label(int n) -> string {
    return describe(n, "n=");
}

fn describe(int n, string prefix) -> string {
    if n > 3 {
        return prefix + "big";
    }
    return prefix + "small";
}

fn first_word(int n) -> Option<int> {
    return later_opt(n);
}

fn later_opt(int n) -> Option<int> {
    if n > 0 {
        return Option::Some(n);
    }
    return Option::None;
}

class Acc {
    pub total: int,
}

impl Acc {
    pub fn add_twice(int n) {
        self.add(n);
        self.add(n);
    }

    pub fn add(int n) {
        self.total = self.total + n;
    }
}

test("forward calls lower") {
    assert(is_even(10))?;
    assert(is_odd(7))?;
    assert(!is_even(3))?;
    assert(label(4) == "n=big")?;
    assert(label(1) == "n=small")?;
    assert((first_word(3) ?? 0) == 3)?;
    assert((first_word(0) ?? -1) == -1)?;
    let a = new Acc(1);
    a.add_twice(5);
    assert(a.total == 11)?;
}
