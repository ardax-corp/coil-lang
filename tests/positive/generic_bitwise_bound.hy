// `<<`, `>>`, `&`, `|`, `^` and `~` on a bound type parameter go through the
// `Shl` / `Shr` / `BitAnd` / `BitOr` / `BitXor` / `BitNot` dictionaries;
// `T: Integral` implies all of them and `Num` (#816, #819-#824). A user
// class overloads them with `impl`, and `-v` lowers through `impl Neg`
// (#812).

fn low_bits<T: Integral>(T x, T mask) -> T {
    return x & mask;
}

fn mix<T: Integral>(T a, T b) -> T {
    return ((a << b) | (a >> b)) ^ ~a;
}

fn integral_arith<T: Integral>(T a, T b) -> T {
    return (a + b) * b % (a ** b);
}

fn shl_only<T: Shl>(T a, T b) -> T {
    return a << b;
}

fn not_only<T: BitNot>(T a) -> T {
    return ~a;
}

fn neg_only<T: Neg>(T a) -> T {
    return -a;
}

fn or_in_place<T: BitOr>(T a, T b) -> T {
    let x = a;
    x |= b;
    return x;
}

class Flags {
    pub bits: int,
}

impl BitAnd for Flags {
    pub fn bitand(Flags a, Flags b) -> Flags {
        return new Flags(a.bits & b.bits);
    }
}

impl BitOr for Flags {
    pub fn bitor(Flags a, Flags b) -> Flags {
        return new Flags(a.bits | b.bits);
    }
}

impl BitXor for Flags {
    pub fn bitxor(Flags a, Flags b) -> Flags {
        return new Flags(a.bits ^ b.bits);
    }
}

impl Shl for Flags {
    pub fn shl(Flags a, Flags b) -> Flags {
        return new Flags(a.bits << b.bits);
    }
}

impl Shr for Flags {
    pub fn shr(Flags a, Flags b) -> Flags {
        return new Flags(a.bits >> b.bits);
    }
}

impl BitNot for Flags {
    pub fn bitnot(Flags a) -> Flags {
        return new Flags(~a.bits & 15);
    }
}

class V {
    pub x: int,
}

impl Neg for V {
    pub fn neg(V a) -> V {
        return new V(-a.x);
    }
}

test("int through Integral") {
    assert(low_bits(255, 15) == 15)?;
    assert(mix(6, 1) == (((6 << 1) | (6 >> 1)) ^ ~6))?;
    assert(integral_arith(2, 3) == (5 * 3) % 8)?;
}

test("int through the single traits") {
    assert(shl_only(1, 4) == 16)?;
    assert(not_only(0) == -1)?;
    assert(not_only(5) == -6)?;
    assert(or_in_place(4, 1) == 5)?;
}

test("byte through the single traits") {
    let b: byte = 12 as byte;
    let m: byte = 10 as byte;
    assert(shl_only(b, 1 as byte) == (b << (1 as byte)))?;
    assert(or_in_place(b, m) == (b | m))?;
}

test("user class bitwise operators") {
    let a = new Flags(12);
    let b = new Flags(10);
    assert((a & b).bits == 8)?;
    assert((a | b).bits == 14)?;
    assert((a ^ b).bits == 6)?;
    assert((a << new Flags(1)).bits == 24)?;
    assert((a >> new Flags(2)).bits == 3)?;
    assert((~a).bits == 3)?;
    let c = new Flags(1);
    c |= new Flags(2);
    c <<= new Flags(1);
    assert(c.bits == 6)?;
    assert(not_only(a).bits == 3)?;
    assert(or_in_place(a, b).bits == 14)?;
}

test("user class negation") {
    let v = -new V(3);
    assert(v.x == -3)?;
    assert(neg_only(new V(4)).x == -4)?;
}
