// Tuple / array literal items built with a nested `new` stay distinct
// objects. The inner constructor's temps sat above the previous item, so
// the next item's `STORE` raised the cursor over a stale inner object and
// `MakeArray` / `MakeTuple` collected it instead of the earlier item.
class B {
    pub v: int,
}

class A {
    pub b: B,
}

fn digits(Vec<A> xs) -> int {
    let n = 0;
    for a in xs {
        n = n * 10 + a.b.v;
    }
    return n;
}

fn pair_digits((A, A) p) -> int {
    let (x, y) = p;
    return x.b.v * 10 + y.b.v;
}

fn arr_digits([A; 3] a) -> int {
    return a[0].b.v * 100 + a[1].b.v * 10 + a[2].b.v;
}

test("Vec::from of an inline literal") {
    let xs: Vec<A> = Vec::from([new A(new B(1)), new A(new B(2)), new A(new B(3))]);
    assert(digits(xs) == 123)?;
    xs[0].b.v = 9;
    assert(xs[1].b.v == 2)?;
}

test("array literal argument") {
    assert(arr_digits([new A(new B(4)), new A(new B(5)), new A(new B(6))]) == 456)?;
}

test("tuple literal") {
    let t = (new A(new B(7)), new A(new B(8)));
    let (x, y) = t;
    assert(x.b.v * 10 + y.b.v == 78)?;
    assert(pair_digits((new A(new B(1)), new A(new B(2)))) == 12)?;
}

test("mixed items") {
    let a = new A(new B(1));
    let xs: Vec<A> = Vec::from([a, new A(new B(2)), a]);
    assert(digits(xs) == 121)?;
}

test("flat item after a nested one") {
    let xs: Vec<A> = Vec::from([new A(new B(1)), new A(new B(2))]);
    let ys: [B; 3] = [xs[0].b, new B(5), new B(6)];
    assert(ys[0].v * 100 + ys[1].v * 10 + ys[2].v == 156)?;
    let zs: Vec<A> = Vec::from([new A(new B(3)), new A(xs[1].b)]);
    assert(digits(zs) == 32)?;
}
