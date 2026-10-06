// Operators over a generic class with derived instances: `Box<int> ==` and
// `<` call the instance with its dictionary, as do the mono clones of a
// `T: Eq` body.
#[derive(Eq, Ord)]
class Box<T> {
    pub item: T,
}

fn same<T: Eq>(T a, T b) -> bool {
    return a == b;
}

fn boxes(Box<int> a, Box<int> b) -> bool {
    return a == b;
}

fn less(Box<int> a, Box<int> b) -> bool {
    return a < b;
}

test("derived Eq on a generic class") {
    assert(boxes(new Box(2), new Box(2)))?;
    assert(!boxes(new Box(2), new Box(3)))?;
}

test("derived Ord on a generic class") {
    assert(less(new Box(2), new Box(3)))?;
    assert(!less(new Box(3), new Box(2)))?;
}

test("bound Eq in mono clones") {
    assert(same(new Box(1), new Box(1)))?;
    assert(!same(new Box("a"), new Box("b")))?;
    assert(same(4, 4))?;
}
