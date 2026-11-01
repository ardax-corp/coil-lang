// A generic function taking a `Vec` of functions that return an open
// `Result` / `Option` (coil-lang#2153): passed, indexed and called at
// ground and generic callers.
use tests::assert;

fn count<T, E>(Vec<unit -> Result<T, E>> bodies) -> int {
    return bodies.len();
}

fn oks<T, E>(Vec<unit -> Result<T, E>> bodies) -> int {
    let n = 0;
    for b in bodies {
        match b() {
            Result::Ok(_) => {
                n = n + 1;
            },
            Result::Err(_) => {},
        }
    }
    return n;
}

fn first_some<T>(Vec<unit -> Option<T>> bodies, T fallback) -> T {
    for b in bodies {
        match b() {
            Option::Some(v) => {
                return v;
            },
            Option::None => {},
        }
    }
    return fallback;
}

fn wrapped<T>(T x) -> int {
    let bodies: Vec<unit -> Result<T, string>> = Vec::new();
    bodies.push(fn () use (x) => Result::Ok(x));
    bodies.push(fn () => Result::Err("no"));
    return oks(bodies);
}

test("a Vec of open Result functions at a ground caller") {
    let fs: Vec<unit -> Result<int, string>> = Vec::new();
    fs.push(fn () => Result::Ok(1));
    fs.push(fn () => Result::Err("x"));
    fs.push(fn () => Result::Ok(3));
    assert!(count(fs) == 3);
    assert!(oks(fs) == 2);
}

test("a Vec of open Option functions") {
    let fs: Vec<unit -> Option<string>> = Vec::new();
    fs.push(fn () => Option::None);
    fs.push(fn () => Option::Some("b"));
    assert!(first_some(fs, "z") == "b");
}

test("a Vec of open Result functions built in a generic caller") {
    assert!(wrapped(5) == 1);
    assert!(wrapped("s") == 1);
}
