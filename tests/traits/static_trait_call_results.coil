// A ground static trait call (`T::default()` in a clone, `Owner::m(v)`)
// boxes no argument, so it is a plain `CALL` of the instance method that
// direct MIR makes too: class, int, string, enum and Option results.
use tests::assert;

#[derive(Default)]
class Q {
    pub x: int,
    pub name: string,
}

#[derive(Default)]
enum Mode {
    Fast,
    Slow,
}

trait Pick<T> {
    static fn pick(int i) -> T {}
}

impl Pick for string {
    pub static fn pick(int i) -> string {
        if i == 0 {
            return "zero";
        }
        return "more";
    }
}

impl Pick for Mode {
    pub static fn pick(int i) -> Mode {
        if i == 0 {
            return Mode::Fast;
        }
        return Mode::Slow;
    }
}

trait Find<T> {
    static fn find(int i) -> Option<T> {}
}

impl Find for Q {
    pub static fn find(int i) -> Option<Q> {
        if i < 0 {
            return Option::None;
        }
        return Option::Some(new Q(i, "found"));
    }
}

fn make<T: Default>() -> T {
    return T::default();
}

fn chosen<T: Pick>(int i) -> T {
    return T::pick(i);
}

test("T::default in clones") {
    let q: Q = make();
    assert!(q.x == 0 && q.name == "");
    let n: int = make();
    assert!(n == 0);
    let m: Mode = make();
    let fast = match m {
        Mode::Fast => true,
        Mode::Slow => false,
    };
    assert!(fast);
}

test("T::pick with an argument") {
    let s: string = chosen(0);
    assert!(s == "zero");
    let m: Mode = chosen(1);
    let slow = match m {
        Mode::Fast => false,
        Mode::Slow => true,
    };
    assert!(slow);
}

test("Owner::m with an Option result") {
    let hit = Q::find(3);
    match hit {
        Option::Some(q) => {
            assert!(q.x == 3 && q.name == "found");
        },
        Option::None => {
            assert!(false, "expected a Q");
        },
    }
    let miss = Q::find(-1);
    assert!(miss == Option::None);
}
