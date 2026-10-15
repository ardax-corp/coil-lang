// Type aliases, plain and parametric (`type Pair<T> = (T, T)`), are the
// same type as what they expand to, in parameters, returns and lets.
use tests::assert;

type Pair<T> = (T, T);

type Id = int;

type Row = (string, int);

type Table = Vec<Row>;

fn swap<T>(Pair<T> p) -> Pair<T> {
    return (p[1], p[0]);
}

fn next_id(Id i) -> Id {
    return i + 1;
}

fn total(Table t) -> int {
    let n = 0;
    for row in t {
        n = n + row[1];
    }
    return n;
}

test("parametric alias at two types") {
    let p: Pair<int> = (1, 2);
    assert!(swap(p) == (2, 1));
    let q: Pair<string> = ("a", "b");
    assert!(swap(q) == ("b", "a"));
    let raw: (int, int) = swap(p);
    assert!(raw[0] == 2);
}

test("scalar alias is the scalar") {
    let i: Id = 5;
    let j: int = next_id(i);
    assert!(j == 6);
}

test("alias of a Vec of rows") {
    let t: Table = Vec::from([("a", 1), ("b", 2)]);
    t.push(("c", 3));
    assert!(total(t) == 6);
}

test("block-local alias shadows nothing outside") {
    type Local = (bool, int);
    let l: Local = (true, 1);
    assert!(l[0]);
}
