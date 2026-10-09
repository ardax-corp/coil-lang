// A shared generic body (a generic class method, or a generic fn reached
// through a forwarded dictionary) keeps a bounded type parameter's values
// as raw words, so what it stores or builds reads back as the value (#802).
use string::format;

class Node<K> {
    pub key: K,
}

class Box2<K> {
    pub items: Vec<K>,
    pub last: Vec<K>,
}

impl Box2<K: Ord + Eq> {
    pub fn put(K k) {
        self.items.push(k);
    }
    pub fn at(int i) -> K {
        return self.items[i];
    }
    pub fn some(K k) -> Option<K> {
        return Option::Some(k);
    }
    pub fn node(K k) -> Node<K> {
        return new Node(k);
    }
    pub fn pair(K k) -> (K, K) {
        return (k, k);
    }
    pub fn arr(K k) -> [K; 2] {
        return [k, k];
    }
    pub fn set0(K k) {
        self.items[0] = k;
    }
}

test("push") {
    let a = new Box2(Vec::new(), Vec::new());
    a.put(3);
    assert(a.items[0] == 3)?;
}

test("return") {
    let a = new Box2(Vec::new(), Vec::new());
    a.items.push(4);
    assert(a.at(0) == 4)?;
}

test("some") {
    let a = new Box2(Vec::new(), Vec::new());
    let r = match a.some(5) {
        Option::Some(x) => x,
        Option::None => 0,
    };
    assert(r == 5)?;
}

test("node") {
    let a = new Box2(Vec::new(), Vec::new());
    assert(a.node(6).key == 6)?;
}

test("pair") {
    let a = new Box2(Vec::new(), Vec::new());
    let (x, y) = a.pair(7);
    assert(x == 7 && y == 7)?;
}

test("arr") {
    let a = new Box2(Vec::new(), Vec::new());
    assert(a.arr(8)[1] == 8)?;
}

test("set") {
    let a = new Box2(Vec::new(), Vec::new());
    a.items.push(1);
    a.set0(9);
    assert(a.items[0] == 9)?;
}

test("float push") {
    let a = new Box2(Vec::new(), Vec::new());
    a.put(1.5);
    assert(a.items[0] == 1.5)?;
    assert(a.at(0) == 1.5)?;
}

fn put<K: Ord>(Vec<K> v, K k) {
    v.push(k);
}

fn fwd<K: Ord>(Vec<K> v, K k) {
    put(v, k);
}

fn first<K: Ord>(Vec<K> v) -> K {
    return v[0];
}

fn fwd_first<K: Ord>(Vec<K> v) -> K {
    return first(v);
}

test("a generic fn through a forwarded dictionary") {
    let v: Vec<int> = Vec::new();
    fwd(v, 3);
    assert(v[0] == 3)?;
    let w: Vec<int> = Vec::new();
    w.push(7);
    assert(fwd_first(w) == 7)?;
}

fn shown<T: Show>(Vec<T> v) -> string {
    return format("%v", v[0]);
}

fn fwd_shown<T: Show>(Vec<T> v) -> string {
    return shown(v);
}

test("Show of a raw word in a shared body") {
    let v: Vec<float> = Vec::new();
    v.push(1.5);
    assert(fwd_shown(v) == "1.5")?;
    let b: Vec<bool> = Vec::new();
    b.push(true);
    assert(fwd_shown(b) == "true")?;
}
