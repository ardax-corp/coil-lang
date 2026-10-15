// A multi-parameter trait is implemented as `impl Trait<B> for A` (the
// head type first) and required with `where Trait<A, B>`; the instance is
// chosen by both types; with several targets for one source type, the
// expected type of a method-form call (`let f: F = c.cast()`) picks it.
use tests::assert;

trait Convert<A, B> {
    fn cast(A x) -> B {}
}

class Celsius {
    pub c: int,
}

class Fahrenheit {
    pub f: int,
}

impl Convert<Fahrenheit> for Celsius {
    fn cast(Celsius x) -> Fahrenheit {
        return new Fahrenheit(x.c * 9 / 5 + 32);
    }
}

impl Convert<int> for Celsius {
    fn cast(Celsius x) -> int {
        return x.c;
    }
}

impl Convert<string> for int {
    fn cast(int x) -> string {
        return "#" + show(x);
    }
}

fn apply_cast<A, B>(A x) -> B where Convert<A, B> {
    return cast(x);
}

test("the expected type of a method call picks the instance") {
    let c = new Celsius(100);
    let f: Fahrenheit = c.cast();
    assert!(f.f == 212);
    let n: int = c.cast();
    assert!(n == 100);
    let s: string = cast(7);
    assert!(s == "#7");
}

test("a where constraint over two types") {
    let s: string = apply_cast(5);
    assert!(s == "#5");
    let f: Fahrenheit = apply_cast(new Celsius(0));
    assert!(f.f == 32);
}
