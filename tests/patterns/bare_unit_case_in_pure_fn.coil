// A bare unit case arm (`None`, `Off`) in a pure recursive function is a
// refutable constructor pattern for the purity and auto-par passes too
// (#1394), so the fork inside it runs only when that case matches.
use tests::assert;

enum Switch {
    On,
    Off,
}

fn pick(int n) -> Option<int> {
    if n <= 1 {
        return Option::Some(n);
    }
    return Option::None;
}

fn fibm(int n) -> int {
    return match pick(n) {
        None => fibm(n - 1) + fibm(n - 2),
        Some(v) => v,
    };
}

fn mode(int n) -> Switch {
    if n <= 1 {
        return Switch::Off;
    }
    return Switch::On;
}

fn fibs(int n) -> int {
    return match mode(n) {
        On => fibs(n - 1) + fibs(n - 2),
        Off => n,
    };
}

test("bare None arm forks only on None") {
    assert!(fibm(0) == 0);
    assert!(fibm(1) == 1);
    assert!(fibm(20) == 6765);
    assert!(fibm(30) == 832040);
}

test("bare user unit case arms") {
    assert!(fibs(1) == 1);
    assert!(fibs(20) == 6765);
    assert!(fibs(30) == 832040);
}
