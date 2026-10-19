// `Vec`, arrays, tuples and records have a structural `Length` instance, as
// `x.len()` reads their length without one: they meet a `T: Length` bound
// through a monomorphic clone and a shared generic body (a `PolyFn` value)
// (#1536).
use tests::assert;

fn size<T: Length>(T x) -> int {
    return x.len();
}

fn twice<T: Length>(T x) -> int {
    return size(x) * 2;
}

test("a Vec or array meets a Length bound") {
    assert!(size("abc") == 3);
    assert!(size(Vec::from([1, 2, 3])) == 3);
    assert!(size([1, 2]) == 2);
    assert!(size(["a"]) == 1);
    let v: Vec<string> = Vec::new();
    assert!(size(v) == 0);
    v.push("x");
    assert!(size(v) == 1);
}

test("a tuple or record meets a Length bound") {
    assert!(size((1, "a", true)) == 3);
    assert!(size({ x: 1, y: 2 }) == 2);
}

test("a Length bound through a nested call") {
    assert!(twice(Vec::from([1, 2])) == 4);
    assert!(twice([1, 2, 3]) == 6);
    assert!(twice((1, 2)) == 4);
}

test("a Length bound through a shared body") {
    let s = size;
    assert!(s(Vec::from([1, 2, 3, 4])) == 4);
    assert!(s([7]) == 1);
    assert!(s((1, 2)) == 2);
    assert!(s("hello") == 5);
}

test("fixed and dynamic arrays share a Length instance") {
    assert!(size([1, 2, 3]) == 3);
    assert!(size([4, 5]) == 2);
    assert!(twice([[1], [2]]) == 4);
    assert!(size([6]) == 1);
    let s = size;
    assert!(s([1, 2, 3, 4, 5]) == 5);
}
