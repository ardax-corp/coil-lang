// #1007: a closure returned by an instance method is called through a local.
use tests::assert;

class Scale {
    pub by: int,
}

impl Scale {
    pub fn times(int k) -> int -> int {
        let by = self.by;
        return fn (int x) use (by, k) => x * by * k;
    }

    pub fn label(string prefix) -> string -> string {
        return fn (string s) use (prefix) => prefix + s;
    }
}

test("closure returned by a method") {
    let s = new Scale(2);
    let t = s.times(3);
    assert!(t(5) == 30);
    assert!(t(1) == 6);
}

test("closure over a string returned by a method") {
    let s = new Scale(1);
    let l = s.label("n=");
    assert!(l("4") == "n=4");
}
