// Assigning a fixed array local to another copies its elements, as a
// stack array's slots are copied: the two never share elements.
fn first([int; 33] a) -> int {
    return a[0];
}

fn sum3([int; 3] a) -> int {
    return a[0] + a[1] + a[2];
}

test("a long array copy") {
    let a = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
        25, 26, 27, 28, 29, 30, 31, 32,
    ];
    let b = a;
    b[0] = 99;
    assert(first(a) == 0, "a unchanged")?;
    assert(first(b) == 99, "b changed")?;
}

test("a reassigned copy") {
    let a = [1, 2, 3];
    let b = [0, 0, 0];
    b = a;
    b[1] = 20;
    assert(sum3(a) == 6, "a unchanged")?;
    assert(sum3(b) == 24, "b changed")?;
}
