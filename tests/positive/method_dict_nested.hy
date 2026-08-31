// Regression: inherent methods under `impl Foo<T: Eq>` need dictionary ABI,
// and nested `self.inner.method(...)` must stage the receiver before arg temps
// (locals share the operand stack).

class Store<T> {
    pub item: T,
    pub present: bool,
}

impl Store<T> {
    pub static fn empty(T dummy) -> Store<T> {
        return new Store(dummy, false);
    }
}

impl Store<T: Eq> {
    pub fn put(T x) -> bool {
        if self.present {
            if self.item == x {
                return false;
            }
        }
        self.item = x;
        self.present = true;
        return true;
    }

    pub fn has(T x) -> bool {
        if self.present {
            return self.item == x;
        }
        return false;
    }
}

class Nest<T> {
    pub inner: Store<T>,
}

impl Nest<T> {
    pub static fn empty(T dummy) -> Nest<T> {
        return new Nest(Store::empty(dummy));
    }
}

impl Nest<T: Eq> {
    pub fn put(T x) -> bool {
        return self.inner.put(x);
    }

    pub fn has(T x) -> bool {
        return self.inner.has(x);
    }
}

test("nested constrained method preserves receiver") {
    let n = Nest::empty(0);
    assert(n.put(1))?;
    assert(n.put(1) == false)?;
    assert(n.has(1))?;
    assert(n.has(2) == false)?;
}

test("nested constrained method with strings") {
    let n = Nest::empty("");
    assert(n.put("a"))?;
    assert(n.put("a") == false)?;
    assert(n.has("a"))?;
    assert(n.has("c") == false)?;
    assert(n.put("b"))?;
    assert(n.has("b"))?;
    assert(n.has("a") == false)?;
}
