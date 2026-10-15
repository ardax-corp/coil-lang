// Two-word values (a tuple kept as a pair) where a heap word is needed, in
// bodies lowered straight from HIR to MIR: compared, wrapped, stored in a
// tuple and matched boxed, as HIR emission does.
use tests::assert;
use string::format;

fn pair(int i) -> (int, int) {
    return (i, i * 10);
}

fn same(int a, int b) -> bool {
    return pair(a) == pair(b);
}

fn differ(int a, int b) -> bool {
    return pair(a) != pair(b);
}

fn wrap(int i) -> Option<(int, int)> {
    if i < 0 {
        return Option::None;
    }
    return Option::Some(pair(i));
}

fn unwrap_sum(int i) -> int {
    return match wrap(i) {
        Option::Some(p) => p[0] + p[1],
        Option::None => -1,
    };
}

fn nest(int i) -> ((int, int), int) {
    return (pair(i), i);
}

fn spread(int i) -> int {
    let (p, k) = nest(i);
    return p[0] + p[1] + k;
}

fn show(int i) -> string {
    return format("%v", pair(i));
}

test("two-word values compare boxed") {
    assert!(same(3, 3));
    assert!(!same(3, 4));
    assert!(differ(1, 2));
    assert!(!differ(5, 5));
}

test("a two-word payload wraps and matches boxed") {
    assert!(unwrap_sum(2) == 22);
    assert!(unwrap_sum(-1) == -1);
}

test("a tuple item holds a two-word value boxed") {
    assert!(spread(1) == 12);
    assert!(spread(4) == 48);
}

test("a two-word format argument goes boxed") {
    assert!(show(2) == format("%v", (2, 20)));
}
