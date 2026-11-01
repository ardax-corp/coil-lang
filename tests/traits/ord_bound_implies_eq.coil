// `Eq` is a superclass of `Ord` (#1716): a `T: Ord` bound allows `==` and
// `!=` on `T`, which call `T`'s `Eq` in the mono clone (a direct call) and
// in the shared body (a call through a function value) alike. A tuple of
// `Ord` items is `Eq` too.
use tests::assert;

#[derive(Eq, Ord)]
class K {
    pub v: int,
}

// `Eq` that ignores `note`: identity and a field-by-field compare both
// differ from it.
#[derive(Ord)]
class Money {
    pub cents: int,
    pub note: int,
}

impl Eq for Money {
    pub fn eq(Money a, Money b) -> bool {
        return a.cents == b.cents;
    }
    pub fn ne(Money a, Money b) -> bool {
        return a.cents != b.cents;
    }
}

fn same<T: Ord>(T a, T b) -> bool {
    return a == b;
}

fn differ<T: Ord>(T a, T b) -> bool {
    return a != b;
}

fn shared_same<T: Ord>(T a, T b) -> bool {
    return a == b;
}

fn shared_differ<T: Ord>(T a, T b) -> bool {
    return a != b;
}

// The largest item and how many items equal it.
fn max_count<T: Ord>(Vec<T> xs) -> (T, int) {
    let best = xs[0];
    for x in xs {
        if x > best {
            best = x;
        }
    }
    let n = 0;
    for x in xs {
        if x == best {
            n += 1;
        }
    }
    return (best, n);
}

test("== under an Ord bound, mono clone") {
    assert!(same(1, 1));
    assert!(!same(1, 2));
    assert!(same("a", "a"));
    assert!(same(1.5, 1.5));
    assert!(same(new K(1), new K(1)));
    assert!(!same(new K(1), new K(2)));
    assert!(same(new Money(1, 2), new Money(1, 3)));
    assert!(differ(new Money(1, 2), new Money(2, 2)));
    assert!(same((1, "a"), (1, "a")));
    assert!(differ((1, "a"), (1, "b")));
}

test("== under an Ord bound, shared body") {
    let f = shared_same;
    assert!(f(1, 1));
    assert!(!f(1, 2));
    assert!(f("a", "a"));
    assert!(f(new K(1), new K(1)));
    assert!(!f(new K(1), new K(2)));
    assert!(f(new Money(1, 2), new Money(1, 3)));
    assert!(!f(new Money(1, 2), new Money(2, 2)));
    let g = shared_differ;
    assert!(!g(new K(1), new K(1)));
    assert!(g(new Money(1, 2), new Money(2, 2)));
    assert!(!g(new Money(1, 2), new Money(1, 3)));
}

test("Ord and Eq together in one body") {
    let ks: Vec<K> = Vec::new();
    ks.push(new K(3));
    ks.push(new K(1));
    ks.push(new K(3));
    let (k, n) = max_count(ks);
    assert!(k.v == 3 && n == 2);
    let ms: Vec<Money> = Vec::new();
    ms.push(new Money(2, 0));
    ms.push(new Money(2, 1));
    ms.push(new Money(1, 0));
    let (m, c) = max_count(ms);
    assert!(m.cents == 2 && c == 2);
}
