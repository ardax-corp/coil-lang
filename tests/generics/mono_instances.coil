// A generic function gives the same results at each type it is called
// with (int, float, string), including when its signature mentions an
// enum of its type parameter.
use tests::assert;

fn first<T>(Vec<T> xs, T fallback) -> T {
    if xs.len() == 0 {
        return fallback;
    }
    return xs[0];
}

fn pick<T>(bool c, T a, T b) -> T {
    if c {
        return a;
    }
    return b;
}

fn count_over<T>(Vec<T> xs, int min) -> int {
    let n = 0;
    let i = 0;
    while i < xs.len() {
        if i >= min {
            n = n + 1;
        }
        i = i + 1;
    }
    return n;
}

fn or_else<T>(Option<T> o, T d) -> T {
    return o ?? d;
}

test("generic clones at int, float and string") {
    let v: Vec<int> = Vec::new();
    v.push(4);
    v.push(5);
    assert!(first(v, 1) == 4);
    assert!(pick(false, 3, 4) == 4);
    assert!(pick(true, 1.5, 2.5) == 1.5);
    assert!(pick(true, "a", "b") == "a");
    assert!(count_over(v, 1) == 1);
}

test("enum boundaries keep the boxed layout") {
    assert!(or_else(Option::Some(3), 7) == 3);
    assert!(or_else(Option::None, 7) == 7);
}
