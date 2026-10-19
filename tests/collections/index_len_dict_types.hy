// What the checker accepts around indexing, `len`, `typeof` and dict
// literals: in-bounds constant and runtime indexes, `Vec` indexes (no
// static bound), `len` / `typeof` result types, `len` through a `Length`
// bound, and dict literals as structural records.
use tests::assert;

fn get_array() -> Vec<int> {
    return [1, 2, 3];
}

// Compiles: a `Vec`'s length is not static, so `arr[10]` is not a
// compile-time error (it would trap if called).
fn far_index() -> int {
    let arr = get_array();
    return arr[10];
}

fn head(Vec<int> arr) -> int {
    return arr[0];
}

fn size_of<T: Length>(T x) -> int {
    return x.len();
}

test("constant and runtime indexes in bounds") {
    let arr = [0, 1, 2];
    assert!(arr[2] == 2);
    let i = 1;
    assert!(arr[i] == 1);
}

test("a Vec index has no static bound") {
    let v = Vec::from([0, 1]);
    v.push(2);
    assert!(v[2] == 2);
    assert!(head(get_array()) == 1);
}

test("len is an int and typeof a string") {
    assert!((typeof [0, 1].len()) == "int");
    assert!([0, 1].len() == 2);
    assert!((typeof "foo".len()) == "int");
    assert!((typeof (1, 2, 3).len()) == "int");
    assert!((typeof ({ a: 1, b: 2 }).len()) == "int");
    assert!((typeof (typeof 1)) == "string");
    assert!((typeof (typeof "hi")) == "string");
    assert!((typeof (typeof (1, 2))) == "string");
}

test("len through a Length bound") {
    assert!(size_of("hi") == 2);
}

test("parentheses without a comma are not a tuple") {
    assert!((typeof (1)) == "int");
    assert!((typeof (1 + 2)) == "int");
    assert!((typeof ((1))) == "int");
    assert!((1 + 2) * 3 == 9);
}

test("dict literals are structural records") {
    let d = { foo: 42 };
    assert!((typeof d) == "{ foo: int }");
    assert!(d.foo == 42);
    let a = { foo: 42 };
    let b = { foo: 99 };
    assert!((typeof a) == (typeof b));
    let p = { x: 1, y: 2 };
    assert!(p.x + p.y == 3);
}
