// LICM must not hoist the LOAD that feeds ArrayLen in `while len(a) < n`.
use tests::assert;

test("while len grows without empty stack") {
    let a: Vec<int> = Vec::new();
    while len(a) < 5 {
        a.push(len(a));
    }
    assert!(len(a) == 5);
    assert!(a[0] == 0);
    assert!(a[4] == 4);
}
