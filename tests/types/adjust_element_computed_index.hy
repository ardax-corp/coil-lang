// #1100: `++xs[i + 1]` / `xs[i + 1]++` used as a value, with a computed
// index: the index runs once, before the element is read and written.
use tests::assert;

fn bump(int i) -> int {
    let xs = [5, 6, 7];
    let b = ++xs[i + 1];
    return b + xs[0];
}

fn post(int i) -> int {
    let xs = [5, 6, 7];
    let a = xs[i * 2]++;
    return a * 100 + xs[i * 2];
}

class Counter {
    pub hits: int,
}

fn next_index(Counter c) -> int {
    c.hits += 1;
    return 1;
}

test("prefix adjust at a computed index") {
    assert!(bump(0) == 12);
    assert!(bump(1) == 13);
}

test("postfix adjust at a computed index") {
    assert!(post(1) == 708);
}

test("index with an effect runs once") {
    let c = new Counter(0);
    let xs = [1, 2, 3];
    let v = --xs[next_index(c)];
    assert!(v == 1);
    assert!(xs[1] == 1);
    assert!(c.hits == 1);
}
