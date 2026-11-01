// LICM must not hoist the LOAD that feeds ArrayLen in `while a.len() < n`.
use tests::assert;

test("while len grows without empty stack") {
    let a: Vec<int> = Vec::new();
    while a.len() < 5 {
        a.push(a.len());
    }
    assert!(a.len() == 5);
    assert!(a[0] == 0);
    assert!(a[4] == 4);
}
