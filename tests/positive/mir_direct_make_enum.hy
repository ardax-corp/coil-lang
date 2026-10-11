// Boxed user enum variants built in bodies lowered straight from HIR to
// MIR: payload words pushed last field first, then `MakeEnum`.
enum Tree {
    Leaf,
    Node(Tree, Tree),
}

enum Shape {
    Dot,
    Line(int),
    Rect(int, float, string),
}

fn build(int depth) -> Tree {
    if depth == 0 {
        return Tree::Leaf();
    }
    return Tree::Node(build(depth - 1), build(depth - 1));
}

fn count(Tree t) -> int {
    return match t {
        Tree::Leaf => 1,
        Tree::Node(l, r) => 1 + count(l) + count(r),
    };
}

fn shape(int k, string name) -> Shape {
    if k == 0 {
        return Shape::Dot();
    }
    if k == 1 {
        return Shape::Line(k * 10);
    }
    let s = Shape::Rect(k, 1.5, name);
    return s;
}

fn score(Shape s) -> int {
    return match s {
        Shape::Dot => 0,
        Shape::Line(n) => n,
        Shape::Rect(w, h, label) => w + len(label) + (h as int),
    };
}

test("boxed enum variants in direct MIR") {
    assert(count(build(4)) == 31)?;
    assert(score(shape(0, "a")) == 0)?;
    assert(score(shape(1, "a")) == 10)?;
    assert(score(shape(3, "abc")) == 7)?;
}
