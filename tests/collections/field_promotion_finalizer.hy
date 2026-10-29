// A finalizer runs at an allocation inside a call, so a loop keeps a field
// in a register across a call only when no `fn drop()` may touch it
// either (`mir::promote_fields`). `Spy::drop` reads `total` of the object
// a static names: the loop must leave `total` in the heap before each
// call that may finalize one, while `count` stays in a register.
// @output seen 30610\ntotal 10 count 4\n
use io::sync::println;
use string::format;
use gc::collect;

// The `total` each finalizer saw, two digits each.
static let seen: int = 0;

static let last: Option<Spy> = Option::None;

class Acc {
    pub total: int,
    pub count: int,
}

static let acc: Acc = new Acc(0, 0);

class Spy {
    pub n: int,
}

impl Spy {
    fn drop() {
        seen = seen * 100 + acc.total;
    }
}

// Replaces the last `Spy`, which is garbage from then on; recurses once
// so it stays a call.
fn spy(int depth) {
    if depth > 0 {
        spy(depth - 1);
        return;
    }
    last = Option::Some(new Spy(depth));
}

// The collection finalizes the previous `Spy` while the loop runs: after
// the second, third and fourth `total` stores.
fn poke(int depth) {
    if depth > 0 {
        poke(depth - 1);
        return;
    }
    spy(1);
    collect();
}

fn run(Acc a, int n) {
    let i = 0;
    while i < n {
        a.total = a.total + i + 1;
        a.count = a.count + 1;
        poke(1);
        i = i + 1;
    }
}

fn main() {
    run(acc, 4);
    let _ = println(format("seen %i", seen));
    let _ = println(format("total %i count %i", acc.total, acc.count));
}
