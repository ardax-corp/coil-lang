// Edge / boundary values that exercise VM encoding and GC-adjacent paths.
use tests::assert;

fn countdown(int n) -> int {
    if n == 0 {
        return 0;
    }
    return countdown(n - 1) + 1;
}

test("negative ints") {
    assert!(0 - 1 + (0 - 2) == 0 - 3);
    assert!((0 - 10) * 2 == 0 - 20);
    assert!(5 - 10 == 0 - 5);
}

test("modulo positive") {
    assert!(7 % 3 == 1);
    assert!(20 % 6 == 2);
}

test("bool as condition edges") {
    let x = 0;
    if !true {
        x = 1;
    }
    assert!(x == 0);
    if !false {
        x = 2;
    }
    assert!(x == 2);
}

test("empty string ops") {
    // Pointer-identity `==`: empty literal equals itself; concat is fresh.
    assert!("" == "");
    let s = "a" + "";
    assert!(s != "");
    let t = "" + "b";
    assert!(t != s);
}

test("single element aggregates") {
    let a = [42];
    assert!(a[0] == 42);
    assert!(len(a) == 1);
    let t = (99,);
    assert!(t[0] == 99);
}

test("many locals") {
    let a0 = 0;
    let a1 = 1;
    let a2 = 2;
    let a3 = 3;
    let a4 = 4;
    let a5 = 5;
    let a6 = 6;
    let a7 = 7;
    let a8 = 8;
    let a9 = 9;
    assert!(a0 + a1 + a2 + a3 + a4 + a5 + a6 + a7 + a8 + a9 == 45);
}

test("deep recursion stack") {
    assert!(countdown(50) == 50);
}

test("alloc churn with arrays") {
    let i = 0;
    let last = 0;
    while i < 32 {
        let a = [i, i + 1, i + 2];
        last = a[0] + a[1] + a[2];
        i = i + 1;
    }
    assert!(last == 31 + 32 + 33);
}
