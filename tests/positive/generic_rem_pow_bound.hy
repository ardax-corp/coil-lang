// `%` and `**` on a bound type parameter go through the `Rem` / `Pow`
// dictionaries (`T: Num` implies both, #815); a user class overloads them
// with `impl Rem` / `impl Pow` (#817, #818). Compound assignment takes the
// same path.

fn wrap<T: Num>(T x, T n) -> T {
    return x % n;
}

fn power<T: Pow>(T x, T n) -> T {
    return x ** n;
}

fn rem_in_place<T: Rem>(T x, T n) -> T {
    let r = x;
    r %= n;
    return r;
}

fn sum_then_power<T: Num>(T a, T b) -> T {
    let x = a;
    x += b;
    x **= b;
    return x;
}

class Mod7 {
    pub v: int,
}

impl Rem for Mod7 {
    pub fn rem(Mod7 a, Mod7 b) -> Mod7 {
        return new Mod7(a.v % b.v);
    }
}

impl Pow for Mod7 {
    pub fn pow(Mod7 a, Mod7 b) -> Mod7 {
        return new Mod7((a.v ** b.v) % 7);
    }
}

test("int and float through a Num bound") {
    assert(wrap(7, 3) == 1)?;
    assert(wrap(7.5, 2.0) == 1.5)?;
}

test("a Pow bound alone") {
    assert(power(3, 2) == 9)?;
    assert(power(2.0, 3.0) == 8.0)?;
}

test("compound assignment through a bound") {
    assert(rem_in_place(10, 4) == 2)?;
    assert(sum_then_power(1, 2) == 9)?;
    assert(sum_then_power(1.0, 2.0) == 9.0)?;
}

test("user class operators") {
    let m = new Mod7(7) % new Mod7(4);
    assert(m.v == 3)?;
    let p = new Mod7(3) ** new Mod7(3);
    assert(p.v == 6)?;
    let q = new Mod7(9);
    q %= new Mod7(5);
    assert(q.v == 4)?;
    assert(rem_in_place(m, new Mod7(2)).v == 1)?;
}
