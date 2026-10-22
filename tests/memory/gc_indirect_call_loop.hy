// Heap values a loop holds across calls through a closure or a trait
// dictionary (`CallIndirect`) survive a `collect` inside the callee: those
// frames get precise maps, as the call's one-word result is exact.
use tests::assert;
use gc::collect;

class Box {
    pub v: int,
}

trait Grow<T> {
    fn grow(T x, int n) -> Box {}
}

impl Grow for Box {
    pub fn grow(Box x, int n) -> Box {
        let junk = new Box(-1);
        collect();
        if junk.v == n {
            return junk;
        }
        return new Box(x.v + n);
    }
}

fn sum_through<T: Grow>(T seed, int n) -> int {
    let out: Vec<Box> = Vec::new();
    let i = 0;
    while i < n {
        out.push(seed.grow(i));
        i = i + 1;
    }
    let total = 0;
    for b in out {
        total = total + b.v;
    }
    return total;
}

test("a dictionary call in a loop keeps the loop's heap values") {
    assert!(sum_through(new Box(10), 5) == 60);
}

test("a closure call in a loop keeps the loop's heap values") {
    let make = fn (int n) {
        collect();
        return new Box(n * 2);
    };
    let kept: Vec<Box> = Vec::new();
    let i = 0;
    while i < 4 {
        let b = make(i);
        kept.push(b);
        i = i + 1;
    }
    assert!(kept[0].v + kept[1].v + kept[2].v + kept[3].v == 12);
}
