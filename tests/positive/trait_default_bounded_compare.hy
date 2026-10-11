// A trait default body may compare two `T`s when the trait's parameter is
// bounded by `Ord`; only comparing a `T` with a concrete type is rejected
// (#838, see tests/compile_fail/type_param_rigid_trait_default.hy).

trait Pick<T: Ord> {
    fn larger(T a, T b) -> T {
        if a > b {
            return a;
        }
        return b;
    }

    fn tag(T x) -> int {}
}

impl Pick for int {
    pub fn tag(int x) -> int {
        return x;
    }
}

impl Pick for float {
    pub fn tag(float f) -> int {
        return 0;
    }
}

test("bounded default compares two T values") {
    assert(larger(3, 7) == 7)?;
    assert(larger(9, 2) == 9)?;
    assert(larger(0, -5) == 0)?;
    assert(larger(1.5, -2.5) == 1.5)?;
}
