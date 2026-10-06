// `len` of a string literal folds to its byte length; `len` of a static
// collection reads the static, then measures it.
static let items: Vec<int> = Vec::new();

fn fill(int n) {
    for i in 0..n {
        items.push(i);
    }
}

test("len of a string literal") {
    assert(len("foo") == 3)?;
    assert(len("") == 0)?;
    assert(len("a\tb\n") == 4)?;
}

test("len of a static vec") {
    let before = len(items);
    fill(5);
    assert(len(items) == before + 5)?;
}
