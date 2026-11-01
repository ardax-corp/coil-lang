// An inclusive range ending at `int` max visits its last value and ends:
// the step past the end must not overflow (#1171).
use tests::assert;

test("for over an inclusive range ending at int max") {
    let hi = 9223372036854775807;
    let n = 0;
    let last = 0;
    for x in (hi - 2)..=hi {
        n = n + 1;
        last = x;
    }
    assert!(n == 3);
    assert!(last == hi);
}

test("single-value inclusive range at int max") {
    let hi = 9223372036854775807;
    let n = 0;
    for x in hi..=hi {
        n = n + 1;
    }
    assert!(n == 1);
}

test("inclusive range at int max with continue and an assigned variable") {
    let hi = 9223372036854775807;
    let n = 0;
    for x in (hi - 3)..=hi {
        if x == hi - 1 {
            continue;
        }
        x = x - hi;
        n = n + 1;
    }
    assert!(n == 3);
}

test("inclusive range to_vec ending at int max") {
    let hi = 9223372036854775807;
    let v = ((hi - 1)..=hi).to_vec();
    assert!(v.len() == 2);
    assert!(v[1] == hi);
}

test("inclusive range still ends at an ordinary bound") {
    let n = 0;
    for x in 1..=5 {
        n = n + x;
    }
    assert!(n == 15);
}
