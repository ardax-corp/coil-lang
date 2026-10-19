// #1071, #1147: `len` / `.len()` measure any structural operand: a `?`
// result, a local `const`, a string concatenation.
use tests::assert;

fn items(int n) -> Result<Vec<int>, string> {
    if n < 0 {
        return Err("negative");
    }
    let v: Vec<int> = [];
    for i in 0..n {
        v.push(i);
    }
    return Ok(v);
}

fn count(int n) -> Result<int, string> {
    return Ok((items(n)?).len());
}

fn count_method(int n) -> Result<int, string> {
    return Ok((items(n)?).len());
}

test("len of a Result ? operand") {
    assert!(count(3) == Ok(3));
    assert!(count(0) == Ok(0));
    assert!(count(-1) == Err("negative"));
    assert!(count_method(4) == Ok(4));
}

test("len of a local const") {
    const s = "abc";
    assert!(s.len() == 3);
    assert!(s.len() == 3);
}

test("len of a string concatenation") {
    let s = "a";
    assert!((s + "b").len() == 2);
    assert!(("a" + "b").len() == 2);
    assert!(("a" + "bc").len() == 3);
    assert!(("" + "").len() == 0);
}
