// `<`, `<=`, `>`, `>=` order strings byte-wise (UTF-8), and `string`
// is `Ord`, so generic code bounded by `Ord` takes strings (stdlib#48).
use tests::assert;

#[derive(Eq, Ord)]
class Name {
    pub last: string,
    pub first: string,
}

#[derive(Eq, Ord)]
enum Key {
    Word(string),
    Num(int),
}

fn smaller<T: Ord>(T a, T b) -> T {
    if a < b {
        return a;
    }
    return b;
}

fn ordered<T: Ord>(T a, T b) -> bool {
    return a <= b && b >= a && !(a > b);
}

test("operators") {
    assert!("a" < "b");
    assert!(!("b" < "a"));
    assert!("a" <= "a");
    assert!("b" > "a");
    assert!("b" >= "b");
    assert!(!("a" < "a"));
}

test("prefix sorts first") {
    assert!("" < "a");
    assert!("ab" < "abc");
    assert!("abc" > "ab");
    assert!(!("abc" < "ab"));
}

test("byte-wise, not case-folded") {
    assert!("B" < "a");
    assert!("Z" < "a");
    assert!("10" < "9");
}

test("utf-8 bytes order after ascii") {
    assert!("z" < "é");
    assert!("é" < "ü");
}

test("runtime strings") {
    let a = "app" + "le";
    let b = "app" + "ly";
    assert!(a < b);
    assert!(b > a);
    assert!(a <= "apple");
}

test("generic Ord bound") {
    assert!(smaller("pear", "fig") == "fig");
    assert!(ordered("a", "b"));
    assert!(!ordered("b", "a"));
}

test("derived Ord over string fields") {
    assert!(new Name("b", "a") > new Name("a", "z"));
    assert!(new Name("a", "a") < new Name("a", "b"));
    assert!(Word("x") < Word("y"));
    assert!(Word("zz") < Num(0));
}
