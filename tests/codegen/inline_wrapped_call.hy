// A call whose body wraps another call is not inlined; the refused attempt
// must leave nothing behind that clobbers the left operand.
use tests::assert;

fn id(int n) -> int {
    return n;
}

fn wrap_add(int n) -> int {
    return 1 + id(n);
}

fn wrap_sub(int n) -> int {
    return 10 - id(n);
}

test("wrapped calls on both sides of a binop") {
    // (1 + 5) + (10 - 3)
    assert!(wrap_add(5) + wrap_sub(3) == 13);
}
