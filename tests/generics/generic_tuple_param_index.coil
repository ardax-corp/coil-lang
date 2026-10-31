// #1143: a shared generic body reads the items of a `(T, T)` tuple
// parameter (a `PolyFn` call runs the shared body, as `--coverage` does).
use tests::assert;

fn swap<T>((T, T) p) -> (T, T) {
    return (p[1], p[0]);
}

fn second<T>((T, T) p) -> T {
    return p[1];
}

test("swap through a direct call") {
    let q = swap((1, 2));
    assert!(q[0] == 2);
    assert!(q[1] == 1);
}

test("swap through a function value") {
    let g = swap;
    let q = g(("a", "b"));
    assert!(q[0] == "b");
    assert!(q[1] == "a");
}

test("item through a function value") {
    let g = second;
    assert!(g((3, 4)) == 4);
    assert!(g(("x", "y")) == "y");
}
