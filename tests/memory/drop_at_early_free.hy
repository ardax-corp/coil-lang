// A `drop` that keeps nothing of `self` runs where the compiler frees its
// object early: at the end of the block that holds it, before the code
// after the block, a parent's before what it holds. A `drop` that keeps
// `self` is left to the collector. A panic in a `drop` run there unwinds
// from the free like any call.
use tests::assert;
use gc::collect;
use task::{scope, Scope, TaskError};

static let LOG: string = "";

fn say(string s) {
    LOG = LOG + s;
}

class Res {
    pub name: string,
}

impl Res {
    fn drop() {
        say("[" + self.name + "]");
    }
}

class Pair {
    pub name: string,
    pub inner: Res,
}

impl Pair {
    fn drop() {
        say("<" + self.name + ">");
    }
}

static let KEPT: Option<Keeper> = Option::None;

class Keeper {
    pub n: int,
}

impl Keeper {
    fn drop() {
        say("(keep)");
        KEPT = Option::Some(self);
    }
}

fn scoped() {
    {
        let r = new Res("a");
        say(r.name);
    }
    say("|");
}

test("a drop runs at the end of the block that holds its object") {
    LOG = "";
    scoped();
    assert!(LOG == "a[a]|", LOG);
}

fn nested() {
    {
        let p = new Pair("p", new Res("r"));
        say(p.name + p.inner.name);
    }
    say("|");
}

test("a parent's drop runs before the drop of what it holds") {
    LOG = "";
    nested();
    assert!(LOG == "pr<p>[r]|", LOG);
}

fn keeps() {
    {
        let k = new Keeper(1);
        say(k.n.show());
    }
    say("|");
}

test("a drop that keeps self is left to the collector") {
    LOG = "";
    keeps();
    assert!(LOG == "1|", LOG);
    collect();
    assert!(LOG == "1|(keep)", LOG);
}

class Loud {
    pub n: int,
}

impl Loud {
    fn drop() {
        say("[loud]");
        panic "in drop";
    }
}

fn loud() -> int {
    defer {
        say("[defer]");
    }
    {
        let l = new Loud(1);
        say(l.n.show());
    }
    say("unreached");
    return 0;
}

fn failed(Result<int, TaskError> r) -> bool {
    return match r {
        Result::Err(TaskError::Panicked(m)) => m == "in drop",
        default => false,
    };
}

test("a panic in a drop run at an early free unwinds from the free") {
    LOG = "";
    assert!(
        failed(
            scope(fn (Scope s) {
                let _ = s.spawn(fn () => loud());
                0
            }),
        ),
    );
    assert!(LOG == "1[loud][defer]", LOG);
}
