// A trait instance for a generic type can bound its parameters:
// `impl Describe for Box<T: Describe>` is an instance for every `Box<X>` with
// `Describe<X>` (its context). The context dictionaries travel inside the
// instance's dictionary, so ground calls, calls through a bound, recursive
// instances and generic callers all reach the right `describe` (#551).

class Box<T> {
    pub item: T,
}

class Pair<A, B> {
    pub a: A,
    pub b: B,
}

enum Tree<T> {
    Leaf(T),
    Node(Tree<T>, Tree<T>),
}

trait Describe<S> {
    fn describe(S x) -> string {}
}

impl Describe for int {
    pub fn describe(int x) -> string {
        return "i";
    }
}

impl Describe for string {
    pub fn describe(string x) -> string {
        return x;
    }
}

impl Describe for Box<T: Describe> {
    pub fn describe(Box<T> b) -> string {
        return "Box(" + b.item.describe() + ")";
    }
}

impl Describe for Pair<A: Describe, B: Describe> {
    pub fn describe(Pair<A, B> p) -> string {
        return "(" + p.a.describe() + "," + p.b.describe() + ")";
    }
}

// The recursive call on a subtree uses the instance's own dictionary.
impl Describe for Tree<T: Describe> {
    pub fn describe(Tree<T> t) -> string {
        let s = match t {
            Tree::Leaf(v) => v.describe(),
            Tree::Node(l, r) => "[" + l.describe() + " " + r.describe() + "]",
        };
        return s;
    }
}

fn show_it<U: Describe>(U x) -> string {
    return x.describe();
}

// The `Describe<Pair<T, int>>` context `Describe<T>` comes from the bound.
fn wrap<T: Describe>(T x) -> string {
    return new Pair(x, 1).describe();
}

test("ground call") {
    assert(new Box(3).describe() == "Box(i)")?;
    assert(new Box("s").describe() == "Box(s)")?;
}

test("nested instances") {
    assert(new Box(new Box(3)).describe() == "Box(Box(i))")?;
    assert(new Pair(new Box(1), "z").describe() == "(Box(i),z)")?;
}

test("dictionary through a bound") {
    assert(show_it(new Box(3)) == "Box(i)")?;
    assert(show_it(new Box(new Box("q"))) == "Box(Box(q))")?;
}

test("two-parameter head") {
    assert(new Pair(1, "s").describe() == "(i,s)")?;
}

test("recursive generic enum") {
    let t: Tree<int> = Tree::Node(Tree::Leaf(1), Tree::Node(Tree::Leaf(2), Tree::Leaf(3)));
    assert(t.describe() == "[i [i i]]")?;
}

test("context from the caller's bound") {
    assert(wrap("q") == "(q,i)")?;
    assert(wrap(new Box(2)) == "(Box(i),i)")?;
}
