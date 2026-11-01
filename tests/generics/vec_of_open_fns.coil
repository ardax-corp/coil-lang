// A generic fn taking a `Vec` of function values over its type parameter
// (coil-lang#2136): the list is stored, iterated and each element called.
use tests::assert;

fn count<T>(Vec<unit -> T> bodies) -> int {
    return bodies.len();
}

fn run_all<T>(Vec<unit -> T> bodies) -> Vec<T> {
    let out: Vec<T> = Vec::new();
    for b in bodies {
        out.push(b());
    }
    return out;
}

fn first<T>(Vec<unit -> T> bodies) -> T {
    let b = bodies[0];
    return b();
}

fn apply_all<T>(Vec<T -> T> fs, T x) -> T {
    let v = x;
    for f in fs {
        v = f(v);
    }
    return v;
}

test("a Vec of open function values is passed, iterated and called") {
    let fs: Vec<unit -> string> = Vec::new();
    fs.push(fn () => "a");
    fs.push(fn () => "b");
    assert!(count(fs) == 2);
    let got = run_all(fs);
    assert!(got.len() == 2);
    assert!(got[0] == "a");
    assert!(got[1] == "b");
    assert!(first(fs) == "a");
}

test("the element type may be an int or an enum") {
    let ints: Vec<unit -> int> = Vec::new();
    ints.push(fn () => 41);
    assert!(first(ints) + 1 == 42);
    let opts: Vec<unit -> Option<int>> = Vec::new();
    opts.push(fn () => Option::Some(7));
    opts.push(fn () => Option::None);
    let got = run_all(opts);
    assert!(got[0] == Option::Some(7));
    assert!(got[1] == Option::None);
    let res: Vec<unit -> Result<int, string>> = Vec::new();
    res.push(fn () => Result::Ok(3));
    res.push(fn () => Result::Err("no"));
    let r = run_all(res);
    assert!(r[0] == Result::Ok(3));
    assert!(r[1] == Result::Err("no"));
}

test("functions taking the type parameter run in order") {
    let fs: Vec<int -> int> = Vec::new();
    fs.push(fn (int x) => x + 1);
    fs.push(fn (int x) => x * 10);
    assert!(apply_all(fs, 4) == 50);
}

fn run_twice<T>(Vec<unit -> T> bodies) -> int {
    let a = run_all(bodies);
    let b = run_all(bodies);
    return a.len() + b.len();
}

fn wrap<T>(unit -> T body) -> Vec<unit -> T> {
    let fs: Vec<unit -> T> = Vec::new();
    fs.push(body);
    fs.push(body);
    return fs;
}

test("a generic caller passes the list on, or builds it") {
    let fs: Vec<unit -> Option<int>> = Vec::new();
    fs.push(fn () => Option::Some(1));
    assert!(run_twice(fs) == 2);
    let got = run_all(wrap(fn () => Option::Some(9)));
    assert!(got.len() == 2);
    assert!(got[1] == Option::Some(9));
}
