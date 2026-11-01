// `symbol::next_id()` hands out the next value of one process-wide counter:
// consecutive in call order, and never the same id twice across threads.
use tests::assert;
use symbol::next_id;
use thread::{spawn, join};

fn take(int n) -> Vec<int> {
    let out = Vec::new();
    let i = 0;
    while i < n {
        out.push(next_id());
        i += 1;
    }
    return out;
}

/// Every id after the one before it.
fn rising(Vec<int> xs) -> bool {
    let i = 1;
    while i < xs.len() {
        if xs[i] <= xs[i - 1] {
            return false;
        }
        i += 1;
    }
    return true;
}

/// No id in both of two rising lists.
fn disjoint(Vec<int> xs, Vec<int> ys) -> bool {
    let i = 0;
    let j = 0;
    while i < xs.len() && j < ys.len() {
        if xs[i] == ys[j] {
            return false;
        }
        if xs[i] < ys[j] {
            i += 1;
        } else {
            j += 1;
        }
    }
    return true;
}

// One case: the runner runs cases (and files) side by side in one process,
// and the counter is process-wide, so another case's calls would land
// between these.
test("ids count up by one and never repeat across threads") {
    let a = next_id();
    let b = next_id();
    let c = next_id();
    assert!(a >= 1);
    assert!(b == a + 1);
    assert!(c == b + 1);

    // A call in a loop in another function counts on.
    let ids = take(100);
    assert!(ids[0] == c + 1);
    let i = 1;
    while i < ids.len() {
        assert!(ids[i] == ids[i - 1] + 1);
        i += 1;
    }

    let t1 = spawn(fn () => take(500))?;
    let t2 = spawn(fn () => take(500))?;
    let mine = take(500);
    let xs = join(t1)?;
    let ys = join(t2)?;
    assert!(mine[0] > ids[99]);
    assert!(rising(mine) && rising(xs) && rising(ys));
    assert!(disjoint(mine, xs));
    assert!(disjoint(mine, ys));
    assert!(disjoint(xs, ys));
}
