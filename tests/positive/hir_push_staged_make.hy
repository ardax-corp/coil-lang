// A `Vec` push of a multi-field variant whose fields compute, and a variant
// nested in another's fields, lower from HIR: the fields stage through temps.
enum Shape {
    Tri(int, int, int),
    Quad(int, int, int, int),
}

enum Tree {
    Leaf,
    Node(int, Tree, Tree),
}

fn perimeter(Shape s) -> int {
    return match s {
        Shape::Tri(a, b, c) => a + b + c,
        Shape::Quad(a, b, c, d) => a + b + c + d,
    };
}

fn build(int n) -> Vec<Shape> {
    let shapes: Vec<Shape> = Vec::with_capacity(n);
    let i = 0;
    while i < n {
        if i % 2 == 0 {
            shapes.push(Shape::Tri(i, i + 1, i + 2));
        } else {
            shapes.push(Shape::Quad(i, 2, 3, i % 5));
        }
        i = i + 1;
    }
    return shapes;
}

fn sum_tree(Tree t) -> int {
    return match t {
        Tree::Leaf => 0,
        Tree::Node(v, l, r) => v + sum_tree(l) + sum_tree(r),
    };
}

fn tree(int k) -> Tree {
    let t = Tree::Node(
        k,
        Tree::Node(k + 1, Tree::Leaf, Tree::Leaf),
        Tree::Node(k * 2, Tree::Leaf, Tree::Leaf),
    );
    return t;
}

test("push of computed variants") {
    let shapes = build(4);
    assert(len(shapes) == 4)?;
    assert(perimeter(shapes[0]) == 3)?;
    assert(perimeter(shapes[1]) == 1 + 2 + 3 + 1)?;
    assert(perimeter(shapes[2]) == 2 + 3 + 4)?;
    assert(perimeter(shapes[3]) == 3 + 2 + 3 + 3)?;
}

test("variant nested in variant fields") {
    assert(sum_tree(tree(5)) == 5 + 6 + 10)?;
}
