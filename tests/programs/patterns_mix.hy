// Recursive enum patterns, integer literal arms, `default`, tuple and record destructuring, `if let` and `while let`.
// @output leaf 3 big pair:3 some-small other 2 x=1\n
use io::sync::println;
use string::format;

enum Tree {
    Leaf,
    Node(int, Tree, Tree),
}

fn depth(Tree t) -> int {
    return match t {
        Tree::Leaf => 0,
        Tree::Node(_, l, r) => {
            let a = depth(l);
            let b = depth(r);
            1 + (if a > b {
                a
            } else {
                b
            })
        },
    };
}

fn describe(Option<int> o) -> string {
    return match o {
        Option::Some(n) => if n < 10 {
            "some-small"
        } else {
            "some-big"
        },
        Option::None => "none",
    };
}

fn main() {
    let t = Tree::Node(
        1,
        Tree::Node(2, Tree::Leaf, Tree::Leaf),
        Tree::Node(3, Tree::Node(4, Tree::Leaf, Tree::Leaf), Tree::Leaf),
    );
    let leaf = match Tree::Leaf {
        Tree::Leaf => "leaf",
        default => "node",
    };
    let n = 150;
    let size = match n {
        0 => "zero",
        1 => "one",
        default => if n > 100 {
            "big"
        } else {
            "mid"
        },
    };
    let pair = (1, 2);
    let (pa, pb) = pair;
    let ps = format("pair:%i", pa + pb);
    let r: Result<int, string> = Result::Err("boom");
    let rs = match r {
        Result::Ok(v) => format("%i", v),
        Result::Err(_) => "other",
    };
    let rec = { x: 1, y: 2 };
    let { x, y } = rec;
    let cnt = 0;
    let opt = Option::Some(5);
    if let Option::Some(v) = opt {
        cnt += v - 3;
    }
    let stack: Vec<int> = Vec::new();
    stack.push(1);
    stack.push(2);
    while let Option::Some(_) = stack.pop() {
        cnt += 0;
    }
    let _ = println(
        format(
            "%s %i %s %s %s %s %i x=%i",
            leaf,
            depth(t),
            size,
            ps,
            describe(Option::Some(3)),
            rs,
            cnt,
            x + y - 2,
        ),
    );
}
