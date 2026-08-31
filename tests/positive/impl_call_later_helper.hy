// COI-109: inherent methods may call module helpers defined later in the file.
class Counter {
    pub value: int,
}

impl Counter {
    pub fn bump() -> int {
        return add_one(self.value);
    }

    pub fn bump_by(int n) -> int {
        return add_n(self.value, n);
    }
}

fn add_one(int n) -> int {
    return n + 1;
}

fn add_n(int n, int k) -> int {
    return n + k;
}

test("method calls later unary helper") {
    let c = new Counter(41);
    assert(c.bump() == 42)?;
}

test("method calls later multi-arg helper") {
    let c = new Counter(10);
    assert(c.bump_by(7) == 17)?;
}
