// Twelve ground types through one generic and one bounded generic: past
// eight types per function the old clone caps sent calls to the shared
// body (boxed arguments, a dictionary per call); each type has its own
// clone now.
//
// @output 54900000
use io::sync::print;
use string::format;

trait Weigh<T> {
    fn weigh(T x) -> int {}
}

class C0 {
    pub v: int,
}

impl Weigh for C0 {
    fn weigh(C0 x) -> int {
        return x.v + 0;
    }
}

class C1 {
    pub v: int,
}

impl Weigh for C1 {
    fn weigh(C1 x) -> int {
        return x.v + 1;
    }
}

class C2 {
    pub v: int,
}

impl Weigh for C2 {
    fn weigh(C2 x) -> int {
        return x.v + 2;
    }
}

class C3 {
    pub v: int,
}

impl Weigh for C3 {
    fn weigh(C3 x) -> int {
        return x.v + 3;
    }
}

class C4 {
    pub v: int,
}

impl Weigh for C4 {
    fn weigh(C4 x) -> int {
        return x.v + 4;
    }
}

class C5 {
    pub v: int,
}

impl Weigh for C5 {
    fn weigh(C5 x) -> int {
        return x.v + 5;
    }
}

class C6 {
    pub v: int,
}

impl Weigh for C6 {
    fn weigh(C6 x) -> int {
        return x.v + 6;
    }
}

class C7 {
    pub v: int,
}

impl Weigh for C7 {
    fn weigh(C7 x) -> int {
        return x.v + 7;
    }
}

class C8 {
    pub v: int,
}

impl Weigh for C8 {
    fn weigh(C8 x) -> int {
        return x.v + 8;
    }
}

class C9 {
    pub v: int,
}

impl Weigh for C9 {
    fn weigh(C9 x) -> int {
        return x.v + 9;
    }
}

class C10 {
    pub v: int,
}

impl Weigh for C10 {
    fn weigh(C10 x) -> int {
        return x.v + 10;
    }
}

class C11 {
    pub v: int,
}

impl Weigh for C11 {
    fn weigh(C11 x) -> int {
        return x.v + 11;
    }
}

fn pick<T>(T a, T b, bool first) -> T {
    if first {
        return a;
    }
    return b;
}

fn total<T: Weigh>(T a, T b) -> int {
    return a.weigh() + b.weigh();
}

fn main() {
    let acc = 0;
    let a0 = new C0(0);
    let b0 = new C0(1);
    let a1 = new C1(1);
    let b1 = new C1(1);
    let a2 = new C2(2);
    let b2 = new C2(1);
    let a3 = new C3(3);
    let b3 = new C3(1);
    let a4 = new C4(4);
    let b4 = new C4(1);
    let a5 = new C5(5);
    let b5 = new C5(1);
    let a6 = new C6(6);
    let b6 = new C6(1);
    let a7 = new C7(7);
    let b7 = new C7(1);
    let a8 = new C8(8);
    let b8 = new C8(1);
    let a9 = new C9(9);
    let b9 = new C9(1);
    let a10 = new C10(10);
    let b10 = new C10(1);
    let a11 = new C11(11);
    let b11 = new C11(1);
    let i = 0;
    while i < 300000 {
        let f = i % 2 == 0;
        acc = acc + total(pick(a0, b0, f), b0);
        acc = acc + total(pick(a1, b1, f), b1);
        acc = acc + total(pick(a2, b2, f), b2);
        acc = acc + total(pick(a3, b3, f), b3);
        acc = acc + total(pick(a4, b4, f), b4);
        acc = acc + total(pick(a5, b5, f), b5);
        acc = acc + total(pick(a6, b6, f), b6);
        acc = acc + total(pick(a7, b7, f), b7);
        acc = acc + total(pick(a8, b8, f), b8);
        acc = acc + total(pick(a9, b9, f), b9);
        acc = acc + total(pick(a10, b10, f), b10);
        acc = acc + total(pick(a11, b11, f), b11);
        i = i + 1;
    }
    let _ = print(format("%i", acc));
}
