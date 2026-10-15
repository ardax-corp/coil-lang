// An operator on a type parameter needs the operator's trait as a bound:
// `T: Num` and `T: Ord` imply `+` and `<`, and `T: Add` / `T: Lt` alone
// are enough for just that operator; a caller's `T: Num` covers a callee's.
use tests::assert;

fn add<T: Num>(T a, T b) -> T {
    return a + b;
}

fn outer<T: Num>(T x) -> T {
    return add(x, x);
}

fn just_add<T: Add>(T a, T b) -> T {
    return a + b;
}

fn less<T: Ord>(T a, T b) -> bool {
    return a < b;
}

fn just_lt<T: Lt>(T a, T b) -> bool {
    return a < b;
}

test("Num and Add bounds allow +") {
    assert!(add(1, 2) == 3);
    assert!((typeof add(1, 2)) == "int");
    assert!(just_add(1, 2) == 3);
    assert!(just_add(1.5, 1.0) == 2.5);
}

test("Ord and Lt bounds allow <") {
    assert!(less(1, 2));
    assert!(!less(2, 1));
    assert!(just_lt(1, 2));
    assert!(!just_lt(2, 2));
}

test("a generic caller's bound covers a generic callee's") {
    assert!(outer(5) == 10);
}
