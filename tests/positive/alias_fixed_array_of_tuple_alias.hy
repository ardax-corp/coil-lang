// #890: an alias of a fixed array whose element is itself an alias.
type P = (string, int);

type T = [P; 2];

test("alias of [tuple alias; N]") {
    let t: T = [("a", 1), ("b", 2)];
    assert(t[1][1] == 2)?;
}
