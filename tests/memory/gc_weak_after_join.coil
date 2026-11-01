// When both branches of an `if`/`else` assign a heap local and the local is
// reassigned again after the join, neither branch's value may stay rooted:
// after a `collect`, a weak to it no longer upgrades, at every optimization
// level and whether or not the function is inlined (#1409).
use tests::assert;
use gc::{collect, upgrade, weak, Weak};

fn gone<T>(Weak<T> w) -> bool {
    return match upgrade(w) {
        Option::Some(_) => false,
        Option::None => true,
    };
}

// Small enough to inline: the caller's frame must not keep either branch's `v`.
fn make(bool c) -> Weak<Vec<int>> {
    let v = Vec::new();
    if c {
        v = Vec::from([1, 2, 3]);
    } else {
        v = Vec::from([4, 5]);
    }
    let w = weak(v);
    v = Vec::new(); // drop the last strong reference
    return w;
}

test("an inlined join value reassigned away is cleared by collect") {
    let a = make(true);
    let b = make(false);
    collect();
    assert!(gone(a) && gone(b));
}

test("the then-branch value is dropped by a rebind after the join") {
    let n = Vec::from([1]);
    let v = Vec::new();
    if n.len() == 1 {
        v = Vec::from([1, 2, 3]);
    } else {
        v = Vec::from([4, 5]);
    }
    let w = weak(v);
    v = Vec::new();
    collect();
    assert!(gone(w));
    assert!(v.len() == 0);
}

test("the else-branch value is dropped by a rebind after the join") {
    let n = Vec::from([1]);
    let v = Vec::new();
    if n.len() == 2 {
        v = Vec::from([1, 2, 3]);
    } else {
        v = Vec::from([4, 5]);
    }
    let w = weak(v);
    v = Vec::new();
    collect();
    assert!(gone(w));
    assert!(v.len() == 0);
}

test("weaks taken inside each branch are cleared after the rebind") {
    let i = 0;
    let ok = true;
    while i < 2 {
        let v = Vec::new();
        let w = weak(v);
        if i == 0 {
            v = Vec::from([1, 2, 3]);
            w = weak(v);
        } else {
            v = Vec::from([4, 5]);
            w = weak(v);
        }
        v = Vec::from([9]);
        collect();
        ok = ok && gone(w) && v.len() == 1;
        i = i + 1;
    }
    assert!(ok);
}

test("a join value rebound to an older value is dropped") {
    let n = Vec::from([1, 2]);
    let v = n;
    if n.len() > 1 {
        v = Vec::from([7]);
    } else {
        v = Vec::from([8]);
    }
    let w = weak(v);
    v = n;
    collect();
    assert!(gone(w));
    assert!(v.len() == 2);
}

test("the join value stays alive while another local holds it") {
    let n = Vec::from([1]);
    let v = Vec::new();
    if n.len() == 1 {
        v = Vec::from([1, 2, 3]);
    } else {
        v = Vec::from([4, 5]);
    }
    let keep = v;
    let w = weak(v);
    v = Vec::new();
    collect();
    assert!(!gone(w));
    assert!(keep.len() == 3 && v.len() == 0);
}

test("a nested join value is dropped by a rebind after the outer join") {
    let n = Vec::from([1, 2]);
    let v = Vec::new();
    if n.len() > 1 {
        if n.len() == 2 {
            v = Vec::from([1, 2, 3]);
        } else {
            v = Vec::from([4]);
        }
    } else {
        v = Vec::from([5, 6]);
    }
    let w = weak(v);
    v = Vec::new();
    collect();
    assert!(gone(w));
    assert!(v.len() == 0);
}
