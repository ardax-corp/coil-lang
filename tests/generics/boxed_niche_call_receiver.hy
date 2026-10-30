// A generic call whose niche `Result` / `Option` comes back as its boxed
// enum object, used straight as a method receiver or argument
// (coil-lang#2154): `mk(3).unwrap_or(d)` reads the call's result, as
// binding it to a local first does.
use tests::assert;
use future::{all};

fn mk<T>(T x) -> Result<Vec<T>, string> {
    let v: Vec<T> = Vec::new();
    v.push(x);
    return Result::Ok(v);
}

fn bad<T>(T x) -> Result<Vec<T>, string> {
    return Result::Err("no");
}

fn opt<T>(T x) -> Option<Vec<T>> {
    let v: Vec<T> = Vec::new();
    v.push(x);
    v.push(x);
    return Option::Some(v);
}

fn len_of(Result<Vec<int>, string> r) -> int {
    return match r {
        Result::Ok(v) => v.len(),
        Result::Err(_) => -1,
    };
}

fn pass<U>(U x) -> U {
    return x;
}

test("chained unwrap_or on a generic call (clone receiver)") {
    let d: Vec<int> = Vec::new();
    let a = mk(3).unwrap_or(d);
    assert!(a.len() == 1);
    assert!(a[0] == 3);
    let b = bad(3).unwrap_or(d);
    assert!(b.len() == 0);
}

test("chained unwrap_or on a generic call (shared receiver)") {
    let bodies: Vec<unit -> int> = Vec::new();
    bodies.push(fn () => 1);
    bodies.push(fn () => 2);
    let d: Vec<int> = Vec::new();
    let out = all(bodies).unwrap_or(d);
    assert!(out.len() == 2);
    assert!(out[0] + out[1] == 3);
}

test("chained Option method on a generic call") {
    let d: Vec<int> = Vec::new();
    assert!(opt(4).unwrap_or(d).len() == 2);
    assert!(opt(4).is_some());
}

test("generic call result as an argument") {
    assert!(len_of(mk(5)) == 1);
    assert!(len_of(bad(5)) == -1);
    assert!(len_of(pass(mk(5))) == 1);
    let d: Vec<int> = Vec::new();
    assert!(pass(mk(6)).unwrap_or(d)[0] == 6);
}
