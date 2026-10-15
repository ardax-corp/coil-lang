// A call through a type parameter's bound gets the trait method's declared
// effects, so a `pure fn` over `T: Area` may call `area(x)` at every type
// with an instance.
use tests::assert;

trait Area<A> {
    pure fn area(A self) -> int {}
}

class Sq {
    pub side: int,
}

class Rect {
    pub w: int,
    pub h: int,
}

impl Area for Sq {
    fn area(Sq self) -> int {
        return self.side * self.side;
    }
}

impl Area for Rect {
    fn area(Rect self) -> int {
        return self.w * self.h;
    }
}

impl Area for int {
    fn area(int self) -> int {
        return self;
    }
}

pure fn sum_areas<T: Area>(Vec<T> xs) -> int {
    let s = 0;
    for x in xs {
        s = s + area(x);
    }
    return s;
}

pure fn bigger<T: Area>(T a, T b) -> int {
    let m = area(a);
    let n = area(b);
    if m > n {
        return m;
    }
    return n;
}

test("bare calls through the bound") {
    let sqs: Vec<Sq> = Vec::from([new Sq(1), new Sq(2)]);
    assert!(sum_areas(sqs) == 5);
    let rs: Vec<Rect> = Vec::from([new Rect(2, 3)]);
    assert!(sum_areas(rs) == 6);
    let ns: Vec<int> = Vec::from([4, 5]);
    assert!(sum_areas(ns) == 9);
}

test("two values of the bound type") {
    assert!(bigger(new Sq(3), new Sq(2)) == 9);
    assert!(bigger(7, 8) == 8);
}
