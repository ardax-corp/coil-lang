// `#[derive(Default)]` generates `static fn default()`; every field takes
// its type's default (`0`, `0.0`, `false`, `""`, `Ty::default()`), and the
// instance is chosen by the annotation when several exist (#524).

#[derive(Default)]
class Inner { pub n: int, pub label: string, }

#[derive(Default)]
class Q { pub x: int, pub f: float, pub ok: bool, pub name: string, pub inner: Inner, }

#[derive(Default)]
class R { pub y: int, }

#[derive(Default)]
enum Shape { Circle(int, float), Square(int) }

#[derive(Default)]
enum Mode { Fast, Slow }

fn make<T: Default>() -> T {
    return T::default();
}

fn is_circle(Shape s) -> bool {
    let r = match s { Shape::Circle(_, _) => true, Shape::Square(_) => false };
    return r;
}

test("Q::default() fills every field with its type's default") {
    let q = Q::default();
    assert(q.x == 0)?;
    assert(q.f == 0.0)?;
    assert(!q.ok)?;
    assert(q.name == "")?;
    assert(q.inner.n == 0)?;
    assert(q.inner.label == "")?;
}

test("annotation chooses among several Default instances") {
    let q: Q = make();
    assert(q.name == "")?;
    let r: R = make();
    assert(r.y == 0)?;
}

test("enum default: first variant with defaulted payload") {
    assert(is_circle(Shape::default()))?;
}

test("enum default: unit variant") {
    let m = Mode::default();
    let fast = match m { Mode::Fast => true, Mode::Slow => false };
    assert(fast)?;
}
