// An array literal passed to a static method's `Vec<T>` parameter builds a
// `Vec`, as it does for a free function (#1992).
use tests::assert;

class R {
    pub n: int,
}

impl R {
    pub static fn make(Vec<string> xs) -> R {
        xs.push("grown");
        return new R(xs.len());
    }

    pub static fn sum(int base, Vec<int> xs) -> int {
        let t = base;
        for x in xs {
            t = t + x;
        }
        return t;
    }

    pub fn add(Vec<int> xs) -> int {
        return self.n + xs.len();
    }
}

fn make_free(Vec<string> xs) -> R {
    return new R(xs.len());
}

test("an array literal into a static method's Vec parameter") {
    let a = make_free(["a", "b"]);
    let b = R::make(["a", "b", "c"]);
    assert!(a.n + b.n == 6);
    assert!(R::sum(1, [2, 3]) == 6);
}

test("an array literal into an inherent method's Vec parameter") {
    let r = new R(3);
    assert!(r.add([1, 2]) == 5);
}
