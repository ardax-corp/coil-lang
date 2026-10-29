// A recursive enum binary search tree: insert, contains, in-order walk into a `Vec`, size, min and height.
// @output [1, 3, 4, 6, 7, 8] 6 true false 3 8\n
use io::sync::println;
use string::format;

enum Bst {
    Empty,
    Node(int, Bst, Bst),
}

fn insert(Bst t, int v) -> Bst {
    return match t {
        Bst::Empty => Bst::Node(v, Bst::Empty, Bst::Empty),
        Bst::Node(x, l, r) => if v < x {
            Bst::Node(x, insert(l, v), r)
        } else if v > x {
            Bst::Node(x, l, insert(r, v))
        } else {
            t
        },
    };
}

fn contains(Bst t, int v) -> bool {
    return match t {
        Bst::Empty => false,
        Bst::Node(x, l, r) => if v == x {
            true
        } else if v < x {
            contains(l, v)
        } else {
            contains(r, v)
        },
    };
}

fn inorder(Bst t, Vec<int> out) {
    match t {
        Bst::Empty => {},
        Bst::Node(x, l, r) => {
            inorder(l, out);
            out.push(x);
            inorder(r, out);
        },
    }
}

fn size(Bst t) -> int {
    return match t {
        Bst::Empty => 0,
        Bst::Node(_, l, r) => 1 + size(l) + size(r),
    };
}

fn min(Bst t) -> Option<int> {
    return match t {
        Bst::Empty => Option::None,
        Bst::Node(x, l, _) => match l {
            Bst::Empty => Option::Some(x),
            default => min(l),
        },
    };
}

fn height(Bst t) -> int {
    return match t {
        Bst::Empty => 0,
        Bst::Node(_, l, r) => {
            let a = height(l);
            let b = height(r);
            1 + (if a > b {
                a
            } else {
                b
            })
        },
    };
}

fn main() {
    let t = Bst::Empty;
    for v in [6, 3, 8, 1, 4, 7, 3] {
        t = insert(t, v);
    }
    let out: Vec<int> = Vec::new();
    inorder(t, out);
    let _ = println(
        format(
            "%v %i %z %z %i %i",
            out,
            size(t),
            contains(t, 7),
            contains(t, 5),
            height(t),
            (min(t) ?? 0) + 7,
        ),
    );
}
