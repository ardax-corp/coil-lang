// A `while` condition is evaluated again before every iteration (with any
// calls in it); `&&` / `||` in it short-circuit; a range's bounds are
// evaluated once; `return` from inside nested loops leaves the function.
use tests::assert;

static let calls: int = 0;

fn limit(int n) -> int {
    calls = calls + 1;
    return n;
}

fn find(Vec<Vec<int>> rows, int want) -> (int, int) {
    let r = 0;
    for row in rows {
        let c = 0;
        while c < len(row) {
            if row[c] == want {
                return (r, c);
            }
            c = c + 1;
        }
        r = r + 1;
    }
    return (-1, -1);
}

test("the condition is re-evaluated each iteration") {
    calls = 0;
    let i = 0;
    while i < limit(3) {
        i = i + 1;
    }
    assert!(i == 3);
    assert!(calls == 4);
}

test("short-circuit in a while condition") {
    calls = 0;
    let xs = [5, 6, 7];
    let i = 0;
    while i < len(xs) && xs[i] != 7 {
        i = i + 1;
    }
    assert!(i == 2);
    let j = 10;
    while j < 3 && limit(1) == 1 {
        j = j + 1;
    }
    assert!(calls == 0);
    let k = 0;
    while k == 0 || limit(0) == 1 {
        k = k + 1;
    }
    assert!(k == 1);
    assert!(calls == 1);
}

test("a compound condition with negation and grouping") {
    let a = 0;
    let b = 10;
    while !(a >= b) && (a < 100 || b < 0) {
        a = a + 2;
        b = b - 1;
    }
    assert!(a == 8 && b == 6);
}

test("range bounds are evaluated once") {
    calls = 0;
    let n = 0;
    for i in limit(0)..limit(4) {
        n = n + 1;
    }
    assert!(n == 4);
    assert!(calls == 2);
}

test("return from nested loops") {
    let rows: Vec<Vec<int>> = [[1, 2], [3, 4, 5], [6]];
    assert!(find(rows, 4) == (1, 1));
    assert!(find(rows, 6) == (2, 0));
    assert!(find(rows, 9) == (-1, -1));
}

test("a while whose body changes the condition's inputs") {
    let v: Vec<int> = [1];
    while len(v) < 5 {
        v.push(v[len(v) - 1] * 2);
    }
    assert!(v[4] == 16);
}
