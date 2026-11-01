// Reading a `readonly` array through an index or a for-in loop is
// allowed; only writes are rejected (#1073).
use tests::assert;

test("index a readonly array literal") {
    let xs = readonly [1, 2, 3];
    assert!(xs[1] == 2);
    let i = 2;
    assert!(xs[i] == 3);
}

test("index in an expression") {
    let xs = readonly [4, 5, 6];
    assert!(xs[0] + xs[1] + xs[2] == 15);
}

test("index a readonly array of strings") {
    let names = readonly ["a", "bc"];
    assert!(names[1] == "bc");
}

test("nested readonly array index") {
    let grid = readonly [[1, 2], [3, 4]];
    assert!(grid[1][0] == 3);
}

test("iterate a readonly array") {
    let xs = readonly [1, 2, 3];
    let sum = 0;
    for x in xs {
        sum += x;
    }
    assert!(sum == 6);
}
