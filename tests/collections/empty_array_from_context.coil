// An empty `[]` takes its type from where it goes (#1876): the payload of
// the left side of `??`, or the parameter it is passed to.
use tests::assert;
use env::args;

fn count(Vec<int> xs) -> int {
    return xs.len();
}

fn fixed([string; 0] xs) -> int {
    return xs.len();
}

fn first_or<T>(Vec<T> xs, T fallback) -> T {
    if xs.len() > 0 {
        return xs[0];
    }
    return fallback;
}

test("coalesce fallback is the payload type") {
    let a = args().ok() ?? [];
    assert!(a.len() >= 0);
    let r: Result<Vec<int>, string> = Err("no");
    let b = r.ok() ?? [];
    b.push(4);
    assert!(b.len() == 1);
    let o: Option<Vec<string>> = Option::None;
    let c = o ?? [];
    assert!(c.len() == 0);
}

test("call argument takes the parameter type") {
    assert!(count([]) == 0);
    assert!(fixed([]) == 0);
    assert!(first_or([], 7) == 7);
}
