// #890: an array literal builds a `Vec<T>` where one is expected, so a
// function over any length takes `Vec<T>` (there is no `[T]`).

fn sum(Vec<int> v) -> int {
    let s = 0;
    for x in v {
        s = s + x;
    }
    return s;
}

test("array literal as Vec in a let") {
    let v: Vec<int> = [1, 2, 3];
    v.push(4);
    assert(v.len() == 4)?;
    assert(sum(v) == 10)?;
}

test("array literal as a Vec argument of any length") {
    assert(sum([4, 5]) == 9)?;
    assert(sum([1, 2, 3, 4, 5, 6]) == 21)?;
}
