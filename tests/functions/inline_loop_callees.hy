// Callees with loops inline: a `break` / `continue` in a spliced loop
// still names the callee's loop, a range local the callee binds stays two
// numbers, and the callee's result is the loop's outcome.
use tests::assert;

fn first_over(int n, int lim) -> int {
    let i = 0;
    while i < n {
        if i * i > lim {
            break;
        }
        i = i + 1;
    }
    return i;
}

fn sum_odd(int n) -> int {
    let acc = 0;
    for x in 0..n {
        if x % 2 == 0 {
            continue;
        }
        acc = acc + x;
    }
    return acc;
}

fn range_sum(int n) -> int {
    let r = 0..n;
    let acc = 0;
    for x in r {
        acc = acc + x;
    }
    return acc;
}

fn vec_sum(Vec<int> v) -> int {
    let acc = 0;
    for x in v {
        acc = acc + x;
    }
    return acc;
}

test("a break in an inlined loop leaves only that loop") {
    let total = 0;
    let k = 0;
    while k < 3 {
        total = total + first_over(100, k * 10);
        k = k + 1;
    }
    assert!(total == 1 + 4 + 5);
}

test("a continue in an inlined for-in skips one step") {
    assert!(sum_odd(10) == 25);
    assert!(sum_odd(0) == 0);
}

test("a range local of an inlined callee") {
    assert!(range_sum(100) == 4950);
    let t = 0;
    let k = 0;
    while k < 3 {
        t = t + range_sum(k);
        k = k + 1;
    }
    assert!(t == 1);
}

test("an inlined for-in over a vec argument") {
    let v: Vec<int> = Vec::new();
    v.push(3);
    v.push(4);
    assert!(vec_sum(v) == 7);
}
