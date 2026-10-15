// A user class with hand-written `Add` … `Pow` instances and an empty
// `impl Num` works with arithmetic operators and through a `T: Num` bound;
// hand-written `Lt` / `Le` / `Gt` / `Ge` plus `impl Ord` give comparisons
// and satisfy `T: Ord`. Compound assignment uses the same instances.
use tests::assert;

class Money {
    pub cents: int,
}

impl Add for Money {
    pub fn add(Money a, Money b) -> Money {
        return new Money(a.cents + b.cents);
    }
}

impl Sub for Money {
    pub fn sub(Money a, Money b) -> Money {
        return new Money(a.cents - b.cents);
    }
}

impl Mul for Money {
    pub fn mul(Money a, Money b) -> Money {
        return new Money(a.cents * b.cents);
    }
}

impl Div for Money {
    pub fn div(Money a, Money b) -> Money {
        return new Money(a.cents / b.cents);
    }
}

impl Rem for Money {
    pub fn rem(Money a, Money b) -> Money {
        return new Money(a.cents % b.cents);
    }
}

impl Pow for Money {
    pub fn pow(Money a, Money b) -> Money {
        return new Money(a.cents ** b.cents);
    }
}

impl Neg for Money {
    pub fn neg(Money a) -> Money {
        return new Money(0 - a.cents);
    }
}

impl Num for Money {
}

impl Lt for Money {
    pub fn lt(Money a, Money b) -> bool {
        return a.cents < b.cents;
    }
}

impl Le for Money {
    pub fn le(Money a, Money b) -> bool {
        return a.cents <= b.cents;
    }
}

impl Gt for Money {
    pub fn gt(Money a, Money b) -> bool {
        return a.cents > b.cents;
    }
}

impl Ge for Money {
    pub fn ge(Money a, Money b) -> bool {
        return a.cents >= b.cents;
    }
}

impl Ord for Money {
}

fn m(int c) -> Money {
    return new Money(c);
}

fn poly<T: Num>(T x, T y) -> T {
    let ny = -y;
    return x * x + y - x / y + x % y - ny;
}

fn biggest<T: Ord>(T a, T b, T c) -> T {
    let best = a;
    if b > best {
        best = b;
    }
    if c >= best {
        best = c;
    }
    return best;
}

fn sum_all<T: Num>(Vec<T> xs, T zero) -> T {
    let s = zero;
    for x in xs {
        s += x;
    }
    return s;
}

test("each arithmetic operator calls its instance") {
    assert!((m(7) + m(3)).cents == 10);
    assert!((m(7) - m(3)).cents == 4);
    assert!((m(7) * m(3)).cents == 21);
    assert!((m(7) / m(3)).cents == 2);
    assert!((m(7) % m(3)).cents == 1);
    assert!((m(2) ** m(5)).cents == 32);
    assert!((-m(4)).cents == 0 - 4);
}

test("precedence is the operators' own") {
    assert!((m(1) + m(2) * m(3)).cents == 7);
    assert!((m(2) ** m(3) ** m(2)).cents == 512);
    assert!((m(10) - m(4) - m(3)).cents == 3);
}

test("comparisons call their instances") {
    assert!(m(1) < m(2));
    assert!(!(m(2) < m(2)));
    assert!(m(2) <= m(2));
    assert!(m(3) > m(2));
    assert!(m(2) >= m(2));
    assert!(!(m(1) >= m(2)));
}

test("a user Num through a bound gives the same result as int") {
    assert!(poly(m(7), m(3)).cents == poly(7, 3));
    assert!(poly(m(0 - 5), m(2)).cents == poly(0 - 5, 2));
}

test("a user Ord through a bound") {
    assert!(biggest(m(4), m(9), m(2)).cents == 9);
    assert!(biggest(m(4), m(4), m(4)).cents == 4);
    assert!(biggest(1, 3, 3) == 3);
}

test("compound assignment on a user type") {
    let a = m(5);
    a += m(2);
    a *= m(3);
    a -= m(1);
    assert!(a.cents == 20);
    let xs: Vec<Money> = Vec::from([m(1), m(2), m(3)]);
    assert!(sum_all(xs, m(0)).cents == 6);
}
