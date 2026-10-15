// A fixed array read from another local and bound or assigned to a local
// is a copy (`vec_from_array`): writes through one never show in the
// other, for scalar and heap elements, across collections.
use tests::assert;
use gc::collect;

fn bumped([int; 3] a, int k) -> int {
    let b = a;
    b[k] = b[k] + 100;
    return a[k] * 1000 + b[k];
}

fn renamed([string; 2] names) -> string {
    let copy = names;
    copy[0] = "z";
    collect();
    return names[0] + copy[0] + copy[1];
}

fn swapped([float; 2] a, [float; 2] b) -> float {
    let c = b;
    c = a;
    c[1] = 9.5;
    return a[1] + b[1] + c[1];
}

test("a bound copy of an int array param") {
    assert!(bumped([1, 2, 3], 0) == 1101);
    assert!(bumped([1, 2, 3], 2) == 3103);
}

test("a copy of a string array survives a collection") {
    assert!(renamed(["a", "b"]) == "azb");
}

test("a reassigned copy") {
    assert!(swapped([1.0, 2.0], [3.0, 4.0]) == 15.5);
}
