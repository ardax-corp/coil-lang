// Allocation churn keeps its checksums: a linked list rebuilt and dropped
// over forced collections, a `Result` whose both payloads are class
// instances, and a host `Result<(), IoError>` in a hot loop.
use tests::assert;
use gc::collect;
use io::result_unit_probe;

class Node {
    pub v: int,
    pub next: Option<Node>,
}

class Cell {
    pub v: int,
}

fn build_list(int n) -> Option<Node> {
    let head: Option<Node> = Option::None;
    let i = n - 1;
    while i >= 0 {
        let node = new Node(i, Option::None);
        node.next = head;
        head = Option::Some(node);
        i = i - 1;
    }
    return head;
}

fn checksum_walk(Option<Node> cur) -> int {
    let acc = 0;
    let go = true;
    while go {
        acc = acc + match cur {
            Option::Some(node) => {
                cur = node.next;
                node.v
            },
            Option::None => {
                go = false;
                0
            },
        };
    }
    return acc;
}

fn churn_round(int n) -> int {
    let head = build_list(n);
    return checksum_walk(head);
}

fn step(int i) -> Result<Cell, Cell> {
    if i % 2 == 0 {
        return Result::Ok(new Cell(13));
    }
    return Result::Err(new Cell(13));
}

test("list churn over forced collections") {
    let acc = 0;
    let r = 0;
    while r < 8 {
        acc = acc + churn_round(125000);
        collect();
        r = r + 1;
    }
    assert!(acc == 62499500000);
}

test("a Result with class payloads on both sides") {
    let acc = 0;
    let i = 0;
    while i < 10000000 {
        acc = acc + match step(i) {
            Result::Ok(n) => n.v,
            Result::Err(e) => e.v,
        };
        i = i + 1;
    }
    assert!(acc == 130000000);
}

test("a host unit Result in a hot loop") {
    let acc = 0;
    let i = 0;
    while i < 20000000 {
        let n = i % 10;
        let code = n;
        if n == 9 {
            code = -1;
        }
        acc = acc + match result_unit_probe(code) {
            Result::Ok(_) => 1,
            Result::Err(_) => 0,
        };
        i = i + 1;
    }
    assert!(acc == 18000000);
}
