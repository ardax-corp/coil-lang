// A collection keeps what suspended coroutines and tasks still reference: a
// paused generator's locals and a sleeping task's captures survive
// `gc::collect`, and their `drop` waits until they are really gone.
use tests::assert;
use gc::collect;
use task::{scope, Scope, TaskError};

static let drops: int = 0;

class Tracked {
    pub n: int,
}

impl Tracked {
    fn drop() {
        drops += 1;
    }
}

fn drop_count() -> int {
    return drops;
}

gen fn holds() {
    let t = new Tracked(4);
    yield 0;
    yield t.n;
}

fn task_holding() -> int {
    let t = new Tracked(6);
    task::sleep(5);
    return t.n;
}

fn run_paused_generator() -> int {
    let h = holds();
    let _ = resume h;
    collect();
    let before = drops;
    let v = resume h;
    return before * 100 + v;
}

test("a paused generator's local survives a collection") {
    drops = 0;
    assert!(run_paused_generator() == 4);
    collect();
    assert!(drops == 1);
}

test("a sleeping task's local survives a collection in its sibling") {
    drops = 0;
    let r = scope(
        fn (Scope s) {
            let t = s.spawn(task_holding);
            task::yield_now();
            collect();
            let during = drop_count();
            let v = match t.join() {
                Result::Ok(v) => v,
                Result::Err(_) => -1,
            };
            during * 100 + v
        },
    );
    assert!(r == Result::Ok(6));
    collect();
    assert!(drops == 1);
}

test("a vector captured by a waiting task keeps its elements") {
    let r = scope(
        fn (Scope s) {
            let t = s.spawn(fn () {
                let v: Vec<string> = Vec::new();
                v.push("a");
                v.push("b");
                task::sleep(5);
                v[0] + v[1]
            });
            task::yield_now();
            collect();
            match t.join() {
                Result::Ok(s) => s,
                Result::Err(_) => "failed",
            }
        },
    );
    let text = match r {
        Result::Ok(s) => s,
        Result::Err(_) => "scope failed",
    };
    assert!(text == "ab");
}
