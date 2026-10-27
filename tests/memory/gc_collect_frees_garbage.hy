// `gc::collect()` returns the bytes it freed: never negative, more than zero
// after garbage was made, and less on a second collect with nothing new.
use tests::assert;
use gc::collect;

// Two locals share the vector, so early freeing leaves it to the GC.
fn garbage() {
    let v: Vec<int> = Vec::new();
    let alias = v;
    let i = 0;
    while i < 10000 {
        v.push(i);
        i += 1;
    }
    if alias.len() != 10000 {
        panic "alias";
    }
}

test("collect is never negative") {
    assert!(collect() >= 0);
    assert!(collect() >= 0);
}

test("collect frees unreachable garbage") {
    collect();
    garbage();
    let freed = collect();
    assert!(freed > 0, "freed something");
    let again = collect();
    assert!(again < freed);
}
