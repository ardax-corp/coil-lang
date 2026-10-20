// Provider for tests/functions/imported_partial_apply.hy: a function and a
// method the test under-applies from another module.

fn add3(int a, int b, int c) -> int {
    return a * 100 + b * 10 + c;
}

class Acc {
    pub k: int,
}

impl Acc {
    pub fn m(int a, int b) -> int {
        return self.k + a * 10 + b;
    }
}
