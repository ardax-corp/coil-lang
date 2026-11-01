// A trait impl body may call the class's inherent methods when the inherent
// `impl` comes after it (also for a `static` method) or is generic.
// An inherent method does not bind its bare name, so a free `fn join`
// stays callable next to `P::join`.
use tests::assert;

class Later {
    pub v: int,
}

class LaterIter {
    pub i: int,
}

impl IntoIterator for Later {
    type Item = int;
    type IntoIter = LaterIter;

    pub fn into_iter(Later m) -> LaterIter {
        return m.iter();
    }
}

impl Later {
    pub fn iter() -> LaterIter {
        return new LaterIter(self.v);
    }
}

impl Iterator for LaterIter {
    type Item = int;

    pub fn next(LaterIter it) -> Option<int> {
        return Option::None;
    }
}

class Stat {
    pub v: int,
}

class StatIter {
    pub i: int,
}

impl IntoIterator for Stat {
    type Item = int;
    type IntoIter = StatIter;

    pub fn into_iter(Stat m) -> StatIter {
        return Stat::make_iter(m);
    }
}

impl Stat {
    pub static fn make_iter(Stat m) -> StatIter {
        return new StatIter(m.v);
    }
}

impl Iterator for StatIter {
    type Item = int;

    pub fn next(StatIter it) -> Option<int> {
        return Option::None;
    }
}

class Map<K, V> {
    pub n: int,
}

class MapIter<K, V> {
    pub n: int,
}

impl Map<K, V> {
    pub fn iter() -> MapIter<K, V> {
        return new MapIter(self.n);
    }
}

impl IntoIterator for Map<K, V> {
    type Item = int;
    type IntoIter = MapIter<K, V>;

    pub fn into_iter(Map<K, V> m) -> MapIter<K, V> {
        return m.iter();
    }
}

impl Iterator for MapIter<K, V> {
    type Item = int;

    pub fn next(MapIter<K, V> it) -> Option<int> {
        return Option::None;
    }
}

class P {
    pub x: int,
}

impl P {
    pub fn join(P other) -> P {
        return other;
    }
}

fn join(string a, string b) -> string {
    return a;
}

test("inherent method declared after the trait impl") {
    let it = new Later(2).into_iter();
    assert!(it.i == 2);
}

test("static inherent method declared after the trait impl") {
    let it = new Stat(3).into_iter();
    assert!(it.i == 3);
}

test("generic inherent method") {
    let m: Map<int, string> = new Map(4);
    let it = m.into_iter();
    assert!(it.n == 4);
}

test("an inherent method does not shadow a free function") {
    let s = join("ok", "");
    assert!((typeof s) == "string");
    assert!(s == "ok");
    let p = new P(1).join(new P(2));
    assert!(p.x == 2);
}
