// Coroutines are traced, not all rooted: a suspended `yield from` parent
// and its delegate survive collections while reachable, and the delegate's
// heap locals survive across yields.
use gc::{collect};

async fn counter() {
    let boxed = [10, 20, 30];
    yield boxed[0];
    collect();
    yield boxed[1];
    collect();
    yield boxed[2];
}

async fn wrap() {
    yield from counter();
}

test("yield from survives collections") {
    let h = wrap();
    let a = resume h;
    collect();
    let b = resume h;
    collect();
    let c = resume h;
    assert(a == 10)?;
    assert(b == 20)?;
    assert(c == 30)?;
}

test("dropped coroutines do not break later ones") {
    let i = 0;
    while i < 100 {
        let _ = counter();
        i = i + 1;
    }
    collect();
    let h = counter();
    let v = resume h;
    assert(v == 10)?;
}
