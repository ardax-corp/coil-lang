// Enum variants of every payload shape: unit (written with or without `()`),
// tuple, record, recursive and generic payloads, built and matched.
use tests::assert;

enum Shape {
    Dot,
    Circle(int),
    Rect(int, int),
    Named { label: string, size: int },
}

fn area(Shape s) -> int {
    return match s {
        Shape::Dot => 0,
        Shape::Circle(r) => 3 * r * r,
        Shape::Rect(w, h) => w * h,
        Shape::Named { label, size } => label.len() + size,
    };
}

fn is_dot(Shape s) -> bool {
    return match s {
        Shape::Dot => true,
        default => false,
    };
}

enum Tree {
    Leaf,
    Node(int, Tree, Tree),
}

fn sum(Tree t) -> int {
    return match t {
        Tree::Leaf => 0,
        Tree::Node(v, l, r) => v + sum(l) + sum(r),
    };
}

fn depth(Tree t) -> int {
    return match t {
        Tree::Leaf => 0,
        Tree::Node(_, l, r) => 1 + max_of(depth(l), depth(r)),
    };
}

fn max_of(int a, int b) -> int {
    if a > b {
        return a;
    }
    return b;
}

enum Box<T> {
    Empty,
    Full(T),
}

fn unwrap_or<T>(Box<T> b, T d) -> T {
    return match b {
        Box::Empty => d,
        Box::Full(v) => v,
    };
}

enum Rank {
    Low,
    Mid,
    High,
}

fn score(Rank r) -> int {
    return match r {
        Rank::Low => 1,
        Rank::Mid => 2,
        Rank::High => 3,
    };
}

test("each payload shape builds and matches") {
    assert!(area(Shape::Dot) == 0);
    assert!(area(Shape::Circle(2)) == 12);
    assert!(area(Shape::Rect(3, 4)) == 12);
    assert!(area(Shape::Named { size: 5, label: "ab" }) == 7);
    assert!(area(Shape::Named { label: "", size: 0 }) == 0);
}

test("a unit variant may be written with empty parens") {
    assert!(is_dot(Shape::Dot()));
    assert!(is_dot(Shape::Dot));
    assert!(!is_dot(Shape::Circle(0)));
    assert!(area(Shape::Dot()) == 0);
}

test("recursive payloads") {
    let t = Tree::Node(
        1,
        Tree::Node(2, Tree::Leaf, Tree::Leaf),
        Tree::Node(3, Tree::Leaf, Tree::Node(4, Tree::Leaf, Tree::Leaf)),
    );
    assert!(sum(t) == 10);
    assert!(depth(t) == 3);
    assert!(sum(Tree::Leaf) == 0);
    assert!(depth(Tree::Leaf) == 0);
}

test("generic payloads at several types") {
    assert!(unwrap_or(Box::Full(7), 0) == 7);
    assert!(unwrap_or(Box::Empty, 5) == 5);
    assert!(unwrap_or(Box::Empty, "d") == "d");
    let b: Box<string> = Box::Full("x");
    assert!(unwrap_or(b, "d") == "x");
    let nested: Box<Box<int>> = Box::Full(Box::Full(3));
    assert!(unwrap_or(unwrap_or(nested, Box::Empty), 0) == 3);
}

test("distinct cases join at the enum type") {
    let rs = [Rank::Low, Rank::High, Rank::Mid];
    let total = 0;
    for r in rs {
        total = total * 10 + score(r);
    }
    assert!(total == 132);
    let pick = Rank::Low;
    if total > 100 {
        pick = Rank::High;
    }
    assert!(score(pick) == 3);
}
