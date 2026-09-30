// A trait instance for a generic type (`impl Describe for Box<T>`) applies
// to every `Box<_>`: the head's bare uppercase letters are its type
// parameters, scoped over the head and the method bodies, and each use
// instantiates them afresh (#550). Bounds on the parameters are #551.

class Box<T> {
    pub item: T,
}

enum Opt<T> {
    Nothing,
    Just(T),
}

trait Describe<S> {
    fn describe(S x) -> string {}
    fn tag(S x) -> int {
        return 1;
    }
}

impl Describe for Box<T> {
    pub fn describe(Box<T> b) -> string {
        let _ = b.item;
        return "Box";
    }
}

impl Describe for Opt<T> {
    pub fn describe(Opt<T> o) -> string {
        let s = match o {
            Opt::Nothing => "Nothing",
            Opt::Just(_) => "Just",
        };
        return s;
    }
    pub fn tag(Opt<T> o) -> int {
        return 2;
    }
}

impl Describe for int {
    pub fn describe(int x) -> string {
        return "int";
    }
}

fn show_it<U: Describe>(U x) -> string {
    return x.describe();
}

fn first<T>(Box<T> b) -> T {
    return b.item;
}

test("generic class instance at several instantiations") {
    assert(new Box(3).describe() == "Box")?;
    assert(new Box("x").describe() == "Box")?;
    assert(describe(new Box(1.5)) == "Box")?;
    assert(new Box(new Box(1)).describe() == "Box")?;
    assert(new Box(1).tag() == 1)?;
}

test("generic enum instance") {
    let n: Opt<int> = Opt::Nothing;
    assert(n.describe() == "Nothing")?;
    let j: Opt<string> = Opt::Just("s");
    assert(j.describe() == "Just")?;
    assert(j.tag() == 2)?;
}

test("generic instance through a bound and next to a concrete one") {
    assert(show_it(new Box(7)) == "Box")?;
    assert(show_it(4) == "int")?;
    assert(first(new Box(9)) == 9)?;
}
