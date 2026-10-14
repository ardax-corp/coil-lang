// A shared generic body works on its bound type parameter's words through
// its dictionary parameters: `%v` of a `T` calls the dictionary's `show`,
// a bound method's `Result<T, E>` comes back as its boxed object, and a
// `T` word stores into a field as it is. Direct MIR does each as HIR
// emission does.
use tests::assert;
use string::format;

trait Parse<T> {
    fn parse(T proto, int n) -> Result<T, string> {}

    fn parse_or(T proto, int n, int d) -> int {
        return match proto.parse(n) {
            Result::Ok(_) => n,
            Result::Err(_) => d,
        };
    }
}

class Seed {
    pub n: int,
}

impl Parse for Seed {
    pub fn parse(Seed proto, int n) -> Result<Seed, string> {
        if n < 0 {
            return Result::Err("negative");
        }
        return Result::Ok(new Seed(n));
    }
}

class Cell<T> {
    pub item: T,
    pub set: bool,
}

impl Cell<T: Eq> {
    pub fn put(T x) -> bool {
        if self.set && self.item == x {
            return false;
        }
        self.item = x;
        self.set = true;
        return true;
    }
}

class Holder<T> {
    pub inner: Cell<T>,
}

impl Holder<T: Eq> {
    pub fn put(T x) -> bool {
        return self.inner.put(x);
    }
}

fn label<T: Show>(Vec<T> v) -> string {
    return format("<%v>", v[0]);
}

fn label_first<T: Show>(Vec<T> v) -> string {
    return label(v);
}

test("bound show of a raw word") {
    let v: Vec<float> = Vec::new();
    v.push(2.5);
    assert!(label_first(v) == "<2.5>");
    let s: Vec<string> = Vec::new();
    s.push("x");
    assert!(label_first(s) == "<x>");
}

test("a bound method's boxed result") {
    assert!(new Seed(0).parse_or(4, 9) == 4);
    assert!(new Seed(0).parse_or(-1, 9) == 9);
}

test("a type parameter word stored into a field") {
    let c = new Cell(0, false);
    assert!(c.put(3));
    assert!(!c.put(3));
    assert!(c.put(4) && c.item == 4);
    let w = new Cell("a", false);
    assert!(w.put("b") && w.item == "b");
    let h = new Holder(new Cell(1, false));
    assert!(h.put(2) && !h.put(2) && h.inner.item == 2);
    let hs = new Holder(new Cell("p", false));
    assert!(hs.put("q") && hs.inner.item == "q");
}
