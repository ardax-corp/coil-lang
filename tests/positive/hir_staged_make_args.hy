// A variant make with several non-trivial arguments stages them through
// temps, so a call holding one stages its own arguments first.
use string::format;

enum T {
    Leaf,
    Node(int, T, T),
}

fn sum(T t) -> int {
    return match t {
        T::Leaf => 0,
        T::Node(v, l, r) => v + sum(l) + sum(r),
    };
}

fn pair(string s, int n) -> string {
    return format("%s%i", s, n);
}

test("nested makes under calls") {
    let s = pair("n", sum(T::Node(1, T::Node(2, T::Leaf(), T::Leaf()), T::Leaf())));
    assert(s == "n3")?;
}

test("make after a live operand") {
    let base = 10;
    assert(base + sum(T::Node(3, T::Leaf(), T::Node(4, T::Leaf(), T::Leaf()))) == 17)?;
}
