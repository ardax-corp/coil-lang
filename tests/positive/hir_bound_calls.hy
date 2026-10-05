// Trait calls on a bound type parameter in a shared generic body go
// through the hidden dictionary: methods, function-style calls and a
// static `T::m(..)` constructor, with int and string results.
trait Size<S> {
    fn size(S x) -> int {}
    fn label(S x, int n) -> string {}
}

class Bag {
    pub n: int,
}

impl Size for int {
    pub fn size(int x) -> int {
        return x;
    }

    pub fn label(int x, int n) -> string {
        return "i" + (x + n).show();
    }
}

impl Size for Bag {
    pub fn size(Bag b) -> int {
        return b.n * 10;
    }

    pub fn label(Bag b, int n) -> string {
        return "bag" + n.show();
    }
}

fn twice<T: Size>(T x) -> int {
    return x.size() + size(x);
}

fn named<T: Size>(T x, int n) -> string {
    let s = x.label(n);
    return s + "!";
}

fn total<T: Size>(T a, T b) -> int {
    let sa = a.size();
    return sa + b.size() * 2;
}

test("bound calls through the dictionary") {
    assert(twice(4) == 8)?;
    assert(twice(new Bag(3)) == 60)?;
    assert(named(5, 2) == "i7!")?;
    assert(named(new Bag(1), 9) == "bag9!")?;
    assert(total(1, 2) == 5)?;
    assert(total(new Bag(1), new Bag(2)) == 50)?;
}
