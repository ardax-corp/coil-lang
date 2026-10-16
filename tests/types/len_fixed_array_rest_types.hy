// `len` of literals and of an empty fixed array is `int`, a `[int; 3]`
// annotation keeps its static length, and a rest parameter has a length.
use tests::assert;

fn count(int... xs) -> int {
    return len(xs);
}

test("len of literals") {
    assert!(len("") == 0);
    assert!(len("abcd") == 4);
    assert!(len([1, 2, 3, 4]) == 4);
    assert!((typeof len("")) == "int");
}

test("fixed arrays keep their length") {
    let xs: [int; 3] = [1, 2, 3];
    assert!((typeof xs) == "[int; 3]");
    let z: [int; 0] = [];
    assert!(len(z) == 0);
}

test("a rest parameter's length") {
    assert!(count(1, 2, 3) == 3);
    assert!((typeof count(1)) == "int");
}
