// Pointer-niche `Option` / `Result` words in bodies lowered straight from
// HIR to MIR: `None` and `Ok(())` are the zero word, `Some` / `Ok` the
// pointer, and a heap `Result`'s `Err` the pointer with its low bit set.
class Node {
    pub v: int,
    pub next: Option<Node>,
}

fn build(int n) -> Option<Node> {
    let head: Option<Node> = Option::None;
    let i = n - 1;
    while i >= 0 {
        head = Option::Some(new Node(i, head));
        i = i - 1;
    }
    return head;
}

fn sum(Option<Node> head) -> int {
    let cur = head;
    let total = 0;
    let go = true;
    while go {
        match cur {
            Option::Some(node) => {
                total = total + node.v;
                cur = node.next;
            },
            Option::None => {
                go = false;
            },
        }
    }
    return total;
}

fn second(Node n) -> Option<Node> {
    return n.next;
}

fn val(Option<Node> o) -> int {
    match o {
        Option::Some(n) => {
            return n.v;
        },
        Option::None => {
            return 0;
        },
    }
}

fn check(Result<(), string> r) -> int {
    match r {
        Result::Ok(_) => {
            return 1;
        },
        Result::Err(_) => {
            return 0;
        },
    }
}

fn find(Option<Node> head, int want) -> Result<Node, string> {
    let cur = head;
    while true {
        match cur {
            Option::Some(node) => {
                if node.v == want {
                    return Result::Ok(node);
                }
                cur = node.next;
            },
            Option::None => {
                return Result::Err("missing");
            },
        }
    }
    return Result::Err("unreachable");
}

fn pick(Result<Node, string> r) -> int {
    match r {
        Result::Ok(n) => {
            return n.v;
        },
        Result::Err(e) => {
            return -len(e);
        },
    }
}

test("niche words in direct MIR") {
    let list = build(5);
    assert(sum(list) == 10)?;
    assert(sum(Option::None) == 0)?;
    let b = new Node(2, Option::None);
    let a = new Node(1, Option::Some(b));
    assert(val(second(a)) == 2)?;
    assert(val(Option::None) == 0)?;
    assert(check(Result::Ok(())) == 1)?;
    assert(check(Result::Err("no")) == 0)?;
    assert(pick(find(list, 3)) == 3)?;
    assert(pick(find(list, 9)) == -7)?;
}
