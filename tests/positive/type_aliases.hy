// type Name = T; aliases in annotations and locals.
type IntPair = (int, int);

type Int3 = [int; 3];

fn sum_pair(IntPair p) -> int {
    return p[0] + p[1];
}

test("alias as parameter type") {
    assert(sum_pair((3, 4)) == 7)?;
}

test("alias for fixed array annotation") {
    let xs: Int3 = [1, 2, 3];
    assert(xs[0] + xs[1] + xs[2] == 6)?;
}

test("local alias shadow in function") {
    type Local = int;
    let x: Local = 9;
    assert(x == 9)?;
}
