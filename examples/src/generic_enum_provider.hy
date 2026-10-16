// A generic enum declared outside the entry file (#1356): one with a
// derived `Show`, one without, each built by a module function.
#[derive(Show)]
enum Tree<T> {
    Leaf(T),
    Empty,
}

enum Slot<T> {
    Full(T),
    Free,
}

fn leaf(int n) -> Tree<int> {
    return Tree::Leaf(n);
}

fn full(int n) -> Slot<int> {
    return Slot::Full(n);
}
