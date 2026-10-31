// Copy-on-write in generic bodies (coil-lang#2161): a value a generic
// method returns or stores is shared like the same code over a concrete
// type, in the per-type clone and in the shared body.
use tests::assert;

class Box {
    pub n: int,
}

class Cell<T> {
    value: T,
}

impl Cell<T> {
    pub fn get() -> T {
        return self.value;
    }

    pub fn set(T v) {
        self.value = v;
    }
}

fn first<T>(Vec<T> xs) -> T {
    return xs[0];
}

fn get_shared<T>(Cell<T> c) -> T {
    return c.get();
}

fn make_counter() -> unit -> int {
    let c = new Cell(new Box(0));
    return fn () use (c) {
        let b = c.get();
        b.n = b.n + 1;
        return b.n;
    };
}

test("a generic getter's result is its own value") {
    let c = new Cell(new Box(0));
    let x = c.get();
    x.n = 5;
    assert!(c.get().n == 0);
    assert!(x.n == 5);
}

test("a generic setter stores its own value") {
    let c = new Cell(new Box(0));
    let b = new Box(1);
    c.set(b);
    b.n = 9;
    assert!(c.get().n == 1);
}

test("a generic free function's result is its own value") {
    let xs: Vec<Box> = Vec::new();
    xs.push(new Box(3));
    let y = first(xs);
    y.n = 4;
    assert!(xs[0].n == 3);
}

test("through a shared generic body") {
    let c = new Cell(new Box(2));
    let x = get_shared(c);
    x.n = 7;
    assert!(c.get().n == 2);
}

test("a counter over a generic cell never writes the captured value") {
    let k = make_counter();
    assert!(k() == 1);
    assert!(k() == 1);
    assert!(k() == 1);
}

test("scalar instances are unchanged") {
    let c = new Cell(5);
    let v = c.get();
    assert!(v == 5);
    c.set(6);
    assert!(c.get() == 6 && v == 5);
}
