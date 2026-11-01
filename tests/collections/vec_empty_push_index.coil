// An empty `Vec` grows by `push` and reads back by index, before and after
// enough pushes to reallocate.
use tests::assert;

test("empty Vec push then index") {
    let arr: Vec<int> = Vec::new();
    arr.push(4);
    arr.push(1);
    arr.push(4);
    assert!(arr.len() == 3);
    assert!(arr[0] == 4);
    assert!(arr[1] == 1);
    assert!(arr[2] == 4);
    let i = 0;
    while i < 80 {
        arr.push(i);
        i = i + 1;
    }
    assert!(arr[0] == 4);
    assert!(arr.len() == 83);
    assert!(arr[82] == 79);
}
