// `gc::collect()` returns the bytes it freed: never negative, more than zero
// after garbage was made, and less on a second collect with nothing new.
use tests::assert;
use gc::collect;

fn garbage() {
    let v: Vec<int> = Vec::new();
    let i = 0;
    while i < 10000 {
        v.push(i);
        i += 1;
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
