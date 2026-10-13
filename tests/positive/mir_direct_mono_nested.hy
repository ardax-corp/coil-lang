// A generic function whose type parameter no bare formal names (only
// inside `Vec<T>` or `Option<T>`, or only in the result) is cloned per
// ground call like one with a bare `T`: the clone's types come from the
// signature against the argument and call types. Its `Option<T>` formals
// arrive in the clone's own layout, with nothing converted on entry.

trait Weigh<T> {
    fn weight(T x) -> int {}
}

impl Weigh for int {
    pub fn weight(int x) -> int {
        return x * 2;
    }
}

impl Weigh for string {
    pub fn weight(string s) -> int {
        return s.len();
    }
}

#[derive(Default)]
class Cell {
    pub n: int,
}

fn total<T: Weigh>(Vec<T> xs) -> int {
    let sum = 0;
    for x in xs {
        sum += x.weight();
    }
    return sum;
}

fn first<K>(Vec<K> v) -> K {
    return v[0];
}

fn make<T: Default>() -> T {
    return T::default();
}

fn get_or<T>(Option<T> o, T d) -> T {
    return match o {
        Option::Some(v) => v,
        Option::None => d,
    };
}

fn pick<T>(Option<T> a, Option<T> b) -> Option<T> {
    return match a {
        Option::Some(_) => a,
        Option::None => b,
    };
}

fn sum_firsts(int n) -> int {
    let acc = 0;
    for i in 0..n {
        let v = Vec::from([i, i + 1]);
        acc += first(v) + total(v);
    }
    return acc;
}

test("a type parameter only inside Vec<T>") {
    assert(total(Vec::from([1, 2, 3])) == 12)?;
    assert(total(Vec::from(["ab", "c"])) == 3)?;
    assert(first(Vec::from([7, 8])) == 7)?;
    assert(first(Vec::from(["x", "y"])) == "x")?;
    assert(sum_firsts(3) == 0 + 2 + 1 + 6 + 2 + 10)?;
}

test("a type parameter only in the result") {
    let n: int = make();
    let s: string = make();
    let c: Cell = make();
    assert(n == 0)?;
    assert(s == "")?;
    assert(c.n == 0)?;
}

test("Option<T> formals pass in the clone's layout") {
    assert(get_or(Option::Some(4), 0) == 4)?;
    assert(get_or(Option::None, 9) == 9)?;
    assert(get_or(Option::Some("a"), "b") == "a")?;
    assert(get_or(Option::None, 1.5) == 1.5)?;
    let c = get_or(Option::Some(new Cell(3)), new Cell(0));
    assert(c.n == 3)?;
    assert(get_or(pick(Option::None, Option::Some(5)), 0) == 5)?;
    assert(get_or(pick(Option::Some(true), Option::None), false))?;
}
