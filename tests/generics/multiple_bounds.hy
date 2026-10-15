// A type parameter can carry several bounds (`T: Num + Ord`), written
// inline or as a `where` clause, and each bound's operators and methods are
// available in the body at every type that satisfies all of them.
use tests::assert;
use string::format;

fn clamp_add<T: Num + Ord>(T a, T b, T hi) -> T {
    let s = a + b;
    if s > hi {
        return hi;
    }
    return s;
}

fn label_max<T: Show + Ord>(T a, T b) -> string {
    if a >= b {
        return format("%v", a);
    }
    return format("%v", b);
}

fn count_eq<T>(Vec<T> xs, T x) -> int where Eq<T> {
    let n = 0;
    for v in xs {
        if v == x {
            n = n + 1;
        }
    }
    return n;
}

#[derive(Show, Eq, Ord)]
class Ver {
    pub n: int,
}

test("Num + Ord at int and float") {
    assert!(clamp_add(2, 3, 10) == 5);
    assert!(clamp_add(8, 3, 10) == 10);
    assert!(clamp_add(0.5, 0.25, 1.0) == 0.75);
}

test("Show + Ord at primitives and a derived class") {
    assert!(label_max(3, 9) == "9");
    assert!(label_max(2.5, 0.5) == "2.5");
    assert!(label_max(new Ver(2), new Ver(1)) == "Ver { n: 2 }");
}

test("a where clause") {
    let xs: Vec<int> = Vec::from([1, 2, 1, 3]);
    assert!(count_eq(xs, 1) == 2);
    let ss: Vec<string> = Vec::from(["a", "b"]);
    assert!(count_eq(ss, "c") == 0);
}
