// `len` of a string literal folds to its byte length; `len` of a static
// collection reads the static, then measures it.
use tests::assert;

static let items: Vec<int> = Vec::new();

fn fill(int n) {
    for i in 0..n {
        items.push(i);
    }
}

test("len of a string literal") {
    assert!("foo".len() == 3);
    assert!("".len() == 0);
    assert!("a\tb\n".len() == 4);
}

test("len of a static vec") {
    let before = items.len();
    fill(5);
    assert!(items.len() == before + 5);
}
