// Function values stored in a `Vec`, a tuple or a fixed array
// (coil-lang#1154).
use tests::assert;

fn inc(int x) -> int {
    return x + 1;
}

fn dbl(int x) -> int {
    return x * 2;
}

test("empty vec of functions") {
    let fs: Vec<int -> int> = [];
    assert!(len(fs) == 0);
}

test("vec of functions, called by index") {
    let fs: Vec<int -> int> = [];
    fs.push(inc);
    fs.push(fn (int x) => x - 3);
    assert!(fs[0](10) == 11);
    assert!(fs[1](10) == 7);
}

test("vec of functions, called in a loop") {
    let fs = [inc, dbl, inc];
    let x = 1;
    for f in fs {
        x = f(x);
    }
    assert!(x == 5);
}

test("tuple holding a function") {
    let p = (inc, 2);
    let (f, n) = p;
    assert!(f(n) == 3);
}

class Handler {
    pub f: int -> int,
}

test("class with a function field") {
    let h = new Handler(inc);
    let g = h.f;
    assert!(g(10) == 11);
    h.f = dbl;
    let k = h.f;
    assert!(k(10) == 20);
}
