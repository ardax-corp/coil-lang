// `gc::heap_bytes()` grows while a large value is pinned with `root` and
// shrinks once it is unrooted and collected.
use tests::assert;
use gc::{collect, heap_bytes, root, unroot, Root};

fn big() -> Vec<int> {
    let v: Vec<int> = Vec::new();
    let i = 0;
    while i < 10000 {
        v.push(i);
        i += 1;
    }
    return v;
}

fn release(Root<Vec<int>> r) {
    let _ = unroot(r);
}

test("heap bytes is never negative, and positive with a pinned value") {
    assert!(heap_bytes() >= 0);
    let r = root(big());
    assert!(heap_bytes() > 0);
    release(r);
}

test("heap bytes follows a pinned value") {
    collect();
    let before = heap_bytes();
    let r = root(big());
    let during = heap_bytes();
    assert!(during > before);
    release(r);
    collect();
    assert!(heap_bytes() < during);
}
