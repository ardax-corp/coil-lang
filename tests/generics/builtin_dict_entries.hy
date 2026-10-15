// The builtin primitive instances (`Add__int__add`, `Show__float__show`,
// `Hash__string__hash`, …) as a shared generic body reaches them: through
// its dictionary, with boxed arguments (a `PolyFn` value).
use tests::assert;

fn arith<T: Num>(T a, T b) -> T {
    return (a + b) * (a - b) / b + a % b;
}

fn neg<T: Neg>(T a) -> T {
    return -a;
}

fn power<T: Num>(T a, T b) -> T {
    return a ** b;
}

fn order<T: Ord>(T a, T b) -> int {
    let n = 0;
    if a < b {
        n += 1;
    }
    if a <= b {
        n += 10;
    }
    if a > b {
        n += 100;
    }
    if a >= b {
        n += 1000;
    }
    return n;
}

fn same<T: Eq>(T a, T b) -> int {
    let n = 0;
    if a == b {
        n += 1;
    }
    if a != b {
        n += 10;
    }
    return n;
}

fn bits<T: Integral>(T a, int n) -> T {
    return ((a << n) | (a >> n)) ^ (a & a) ^ ~a;
}

fn shown<T: Show>(T x) -> string {
    return x.show();
}

fn hashed<T: Hash>(T x) -> int {
    return x.hash();
}

fn zero<T: Default>() -> T {
    return default();
}

fn sized<T: Length>(T x) -> int {
    return x.len();
}

test("arithmetic and order through the dictionary") {
    let f = arith;
    assert!(f(7, 2) == 23);
    assert!(f(7.0, 2.0) == 23.5);
    let n = neg;
    assert!(n(5) == -5);
    assert!(n(1.5) == -1.5);
    let p = power;
    assert!(p(3, 4) == 81);
    assert!(p(2.0, 3.0) == 8.0);
    let o = order;
    assert!(o(1, 2) == 11);
    assert!(o(2, 2) == 1010);
    assert!(o(2.5, 1.5) == 1100);
}

test("equality, bits, show, hash, default and length") {
    let e = same;
    assert!(e("ab", "ab") == 1);
    assert!(e("ab", "cd") == 10);
    assert!(e(true, false) == 10);
    assert!(e(3, 3) == 1);
    let b = bits;
    assert!(b(6, 1) == ((6 << 1) | (6 >> 1)) ^ 6 ^ ~6);
    let s = shown;
    assert!(s(42) == "42");
    assert!(s(2.5) == "2.5");
    assert!(s(true) == "true");
    assert!(s("hi") == "hi");
    let h = hashed;
    assert!(h(9) == 9);
    assert!(h("abc") == h("abc"));
    let z: int = zero();
    let empty: string = zero();
    assert!(z == 0);
    assert!(empty == "");
    let l = sized;
    assert!(l("hello") == 5);
}
