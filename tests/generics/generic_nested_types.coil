// Generic functions over nested generic types (`Vec<Option<T>>`,
// `Option<Vec<T>>`, `(T, Vec<T>)`) and recursive generic functions give
// the same results at each element type.
use tests::assert;

fn somes<T>(Vec<Option<T>> xs) -> Vec<T> {
    let out: Vec<T> = Vec::new();
    for x in xs {
        match x {
            Option::Some(v) => out.push(v),
            Option::None => {},
        }
    }
    return out;
}

fn split_first<T>(Vec<T> xs) -> Option<(T, Vec<T>)> {
    if xs.len() == 0 {
        return Option::None;
    }
    let rest: Vec<T> = Vec::new();
    for i in 1..xs.len() {
        rest.push(xs[i]);
    }
    return Option::Some((xs[0], rest));
}

fn length<T>(Vec<T> xs) -> int {
    return match split_first(xs) {
        Option::None => 0,
        Option::Some(p) => {
            let (_, rest) = p;
            1 + length(rest)
        },
    };
}

fn count_all<T>(Option<Vec<T>> o) -> int {
    return match o {
        Option::Some(v) => v.len(),
        Option::None => 0,
    };
}

test("Vec of Option") {
    let xs: Vec<Option<int>> = Vec::from([Option::Some(1), Option::None, Option::Some(3)]);
    let ys = somes(xs);
    assert!(ys.len() == 2 && ys[0] == 1 && ys[1] == 3);
    let ss: Vec<Option<string>> = Vec::from([Option::None, Option::Some("a")]);
    assert!(somes(ss)[0] == "a");
}

test("a tuple with a Vec inside an Option") {
    let xs: Vec<int> = Vec::from([5, 6, 7]);
    let (h, rest) = split_first(xs) ?? (0, Vec::new());
    assert!(h == 5 && rest.len() == 2);
    let none: Vec<string> = Vec::new();
    assert!(count_all(Option::Some(none)) == 0);
}

test("a recursive generic function") {
    let xs: Vec<int> = Vec::from([1, 2, 3, 4]);
    assert!(length(xs) == 4);
    let none: Vec<int> = Vec::new();
    assert!(length(none) == 0);
}
