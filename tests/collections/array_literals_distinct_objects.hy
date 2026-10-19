// #1373: two array literals with the same items are two objects; pushing
// to or storing into one leaves the other as it was.
use tests::assert;

fn same_item(int x) -> int {
    let a: Vec<int> = [x];
    let b: Vec<int> = [x];
    a.push(3);
    return b.len();
}

fn fill(Vec<int> codes, Vec<int> lens) {
    codes.push(1);
    lens.push(2);
    codes.push(3);
}

test("two empty literals") {
    let a: Vec<int> = [];
    let b: Vec<int> = [];
    a.push(1);
    assert!(a.len() == 1);
    assert!(b.len() == 0);
}

test("two literals of the same local") {
    assert!(same_item(4) == 1);
}

test("a store into one of two equal literals") {
    let x = 7;
    let a: Vec<int> = [x, x];
    let b: Vec<int> = [x, x];
    a[0] = 1;
    assert!(a[0] == 1);
    assert!(b[0] == 7);
}

test("two empty vectors filled through a helper") {
    let codes: Vec<int> = [];
    let lens: Vec<int> = [];
    fill(codes, lens);
    assert!(codes.len() == 2);
    assert!(lens.len() == 1);
    assert!(lens[0] == 2);
}
