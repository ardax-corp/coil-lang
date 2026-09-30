// `#[derive]` on generic types expands to bounded instances
// (`impl Show for Box<T: Show>`, #552): each derive bounds every type
// parameter by the trait its body uses (`Ord` by `Ord + Eq`), so the
// derived instance applies to `Box<X>` whenever `X` has the trait.

#[derive(Show, Eq, Ord, Hash, String, Default)]
class Box<T> {
    pub item: T,
}

#[derive(Show, Eq, Hash)]
class Pair<A, B> {
    pub a: A,
    pub b: B,
}

#[derive(Show, Eq, Ord, Default)]
enum Tree<T> {
    Leaf(T),
    Node(Tree<T>, Tree<T>),
}

#[derive(Eq)]
class Point {
    pub x: int,
}

fn same<T: Eq>(T a, T b) -> bool {
    return a == b;
}

test("Show / String") {
    assert(new Box(3).show() == "Box { item: 3 }")?;
    assert(new Box("s").to_string() == "Box { item: s }")?;
    assert(new Box(new Box(1)).show() == "Box { item: Box { item: 1 } }")?;
    assert(new Pair(1, "x").show() == "Pair { a: 1, b: x }")?;
}

test("Eq on several instantiations, nested, and through a bound") {
    assert(new Box(3) == new Box(3))?;
    assert(new Box(3) != new Box(4))?;
    assert(new Box("a") == new Box("a"))?;
    assert(new Box(new Point(1)) == new Box(new Point(1)))?;
    assert(new Box(new Point(1)) != new Box(new Point(2)))?;
    assert(new Pair(1, "x") == new Pair(1, "x"))?;
    assert(!(new Pair(1, "x") == new Pair(1, "y")))?;
    assert(same(new Box(7), new Box(7)))?;
    assert(!same(new Box(7), new Box(8)))?;
}

test("Ord") {
    assert(new Box(1) < new Box(2))?;
    assert(new Box(2) >= new Box(2))?;
    assert(!(new Box(3) <= new Box(2)))?;
}

test("Hash") {
    assert(new Box(5).hash() == new Box(5).hash())?;
    assert(new Pair(1, "x").hash() == new Pair(1, "x").hash())?;
}

test("Default fills the parameter with its own default") {
    let b: Box<int> = Box::default();
    assert(b.item == 0)?;
    let s: Box<string> = Box::default();
    assert(s.item == "")?;
}

test("recursive generic enum") {
    let t: Tree<int> = Tree::Node(Tree::Leaf(1), Tree::Leaf(2));
    let u: Tree<int> = Tree::Node(Tree::Leaf(1), Tree::Leaf(2));
    let v: Tree<int> = Tree::Node(Tree::Leaf(1), Tree::Leaf(3));
    assert(t == u)?;
    assert(t != v)?;
    assert(t < v)?;
    assert(t.show() == "Tree::Node(Tree::Leaf(1), Tree::Leaf(2))", t.show())?;
    let d: Tree<int> = Tree::default();
    assert(d == Tree::Leaf(0))?;
}
