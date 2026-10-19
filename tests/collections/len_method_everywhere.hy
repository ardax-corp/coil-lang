// `x.len()` reads a structural length in a module `const`, in a
// contract clause, and on a `Vec` in a loop bound.
use tests::assert;

const WORD = "abcd".len();

const ROW = [1, 2, 3].len();

fn byte_at(string s, int i) -> int
    requires i >= 0 && i < s.len()
{
    return s.len() - i;
}

fn first(Vec<int> v) -> int
    requires v.len() > 0
{
    return v[0];
}

fn total(Vec<int> v) -> int {
    let acc = 0;
    let i = 0;
    while i < v.len() {
        acc = acc + v[i];
        i = i + 1;
    }
    return acc;
}

test("const lengths fold") {
    let a: [int; ROW] = [7, 8, 9];
    assert!(WORD == 4);
    assert!(a.len() == 3);
}

test("lengths in contract clauses") {
    assert!(byte_at("hello", 1) == 4);
    let v: Vec<int> = [5, 6];
    assert!(first(v) == 5);
}

test("vec length as a loop bound") {
    let v: Vec<int> = [1, 2, 3, 4];
    assert!(total(v) == 10);
    v.push(5);
    assert!(total(v) == 15);
}
