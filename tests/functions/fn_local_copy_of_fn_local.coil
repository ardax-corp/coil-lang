// A function local initialized from, or reassigned to, another function
// local that is itself callable is called through the copied closure word
// (coil-lang#1783).
use tests::assert;

fn sum(int a, int b) -> int {
    return a + b;
}

fn mid(int a, int b) -> int {
    return (a + b) / 2;
}

fn scale(int k) -> fn(int, int) -> int {
    return fn (int a, int b) use (k) => (a + b) * k;
}

fn copy() -> int {
    let f = sum;
    let g = f;
    return g(1, 2);
}

fn copy_of_copy() -> int {
    let f = sum;
    let g = f;
    let h = g;
    return h(3, 4);
}

fn copy_of_lambda(int k) -> int {
    let f = fn (int a, int b) use (k) => a * b + k;
    let g = f;
    return g(2, 3);
}

fn copy_of_returned(int k) -> int {
    let f = scale(k);
    let g = f;
    return g(1, 2);
}

fn assigned_copy(bool b) -> int {
    let f = sum;
    let g = mid;
    if b {
        g = f;
    }
    return g(2, 8);
}

fn param_copy(fn(int, int) -> int p) -> int {
    let g = p;
    return g(5, 6);
}

fn cycle(int n) -> int {
    let f = sum;
    let g = f;
    let i = 0;
    let total = 0;
    while i < n {
        total = total + g(i, 1);
        f = mid;
        g = f;
        f = g;
        i = i + 1;
    }
    return total + f(10, 20);
}

test("a copy of a function local is callable") {
    assert!(copy() == 3);
    assert!(copy_of_copy() == 7);
    assert!(copy_of_lambda(4) == 10);
    assert!(copy_of_returned(3) == 9);
    assert!(assigned_copy(false) == 5);
    assert!(assigned_copy(true) == 10);
    assert!(param_copy(sum) == 11);
    assert!(cycle(0) == 30);
    assert!(cycle(2) == 17);
}
