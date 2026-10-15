// Type aliases are substituted at typecheck time: a parametric alias
// expands when applied, an alias may name another alias, and an inner
// block may shadow an outer alias for its own scope.
use tests::assert;

type Pair<T> = (T, T);

type Meters = int;

type Span = Pair<Meters>;

type Grid = [[int; 2]; 2];

type Bytes = Vec<byte>;

fn swap<T>(Pair<T> p) -> Pair<T> {
    return (p[1], p[0]);
}

fn length(Span s) -> Meters {
    return s[1] - s[0];
}

test("parametric alias") {
    let p: Pair<int> = (1, 2);
    let q = swap(p);
    assert!(q[0] == 2 && q[1] == 1);
    let s: Pair<string> = ("a", "b");
    assert!(swap(s)[0] == "b");
    assert!((typeof p) == "(int, int)");
}

test("alias of an alias") {
    let s: Span = (3, 10);
    assert!(length(s) == 7);
    let m: Meters = length(s);
    assert!(m + 1 == 8);
}

test("nested and generic aliases") {
    let g: Grid = [[1, 2], [3, 4]];
    assert!(g[1][0] == 3);
    let b: Bytes = Vec::new();
    b.push(7);
    assert!(len(b) == 1);
}

test("inner block shadows an alias") {
    type Unit = int;
    let a: Unit = 1;
    {
        type Unit = string;
        let b: Unit = "s";
        assert!(b == "s");
    }
    let c: Unit = a + 1;
    assert!(c == 2);
}
