// A parametric alias (`type Pair<T> = (T, T)`) expands on application, in
// signatures of generic and concrete functions alike, and nests.
use tests::assert;

type Pair<T> = (T, T);

type Table<K, V> = Vec<(K, V)>;

type Maybe<T> = Option<T>;

fn swap<T>(Pair<T> p) -> Pair<T> {
    let (a, b) = p;
    return (b, a);
}

fn sum_pair(Pair<int> p) -> int {
    let (a, b) = p;
    return a + b;
}

fn lookup<V>(Table<string, V> t, string key) -> Maybe<V> {
    for row in t {
        let (k, v) = row;
        if k == key {
            return Option::Some(v);
        }
    }
    return Option::None;
}

test("an alias in a generic signature") {
    let (a, b) = swap((1, 2));
    assert!(a == 2 && b == 1);
    let (s, t) = swap(("x", "y"));
    assert!(s == "y" && t == "x");
}

test("an alias at a concrete type") {
    let p: Pair<int> = (3, 4);
    assert!(sum_pair(p) == 7);
    assert!(sum_pair(swap(p)) == 7);
}

test("aliases with two parameters and nested aliases") {
    let t: Table<string, int> = Vec::from([("a", 1), ("b", 2)]);
    assert!(lookup(t, "b") == Option::Some(2));
    assert!(lookup(t, "c") == Option::None);
    let names: Table<string, string> = Vec::from([("k", "v")]);
    assert!((lookup(names, "k") ?? "") == "v");
}
