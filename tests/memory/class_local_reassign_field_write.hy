// A class local made by `new` that is later reassigned to another object
// and then has a field written names that other object: the write is seen
// through both names (#1556).
use tests::assert;

class P {
    pub x: int,
}

fn alias_then_write() -> int {
    let p = new P(1);
    let q = new P(5);
    q = p;
    q.x = 9;
    return p.x;
}

fn write_before_and_after() -> int {
    let p = new P(1);
    let q = new P(5);
    q.x = 7;
    let before = q.x;
    q = p;
    q.x = q.x + 40;
    return before * 100 + p.x;
}

fn reassign_in_loop(int n) -> int {
    let a = new P(0);
    let b = new P(100);
    let cur = new P(-1);
    let i = 0;
    while i < n {
        if i % 2 == 0 {
            cur = a;
        } else {
            cur = b;
        }
        cur.x = cur.x + 1;
        i = i + 1;
    }
    return a.x * 1000 + b.x;
}

test("a reassigned class local writes the object it now names") {
    assert!(alias_then_write() == 9);
    assert!(write_before_and_after() == 741);
    assert!(reassign_in_loop(5) == 3102);
}
