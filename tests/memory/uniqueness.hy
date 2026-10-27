// Step 3 of docs/internals/escape-memory.md: a reassigned list frees its old
// graph, a matched string is freed after the match, `from_bytes` takes a
// dead buffer's bytes and an indexed `to_bytes` copy reads the string.
// Every value that is still read must survive collections.
use tests::assert;
use gc::collect;
use string::{from_bytes, to_bytes};

class Node {
    pub v: int,
    pub next: Option<Node>,
}

fn build(int n) -> Option<Node> {
    let head: Option<Node> = Option::None;
    let i = 0;
    while i < n {
        head = Option::Some(new Node(i, head));
        i += 1;
    }
    return head;
}

fn total(Option<Node> head) -> int {
    let s = 0;
    let cur = head;
    let go = true;
    while go {
        match cur {
            Option::Some(node) => {
                s += node.v;
                cur = node.next;
            },
            Option::None => {
                go = false;
            },
        }
    }
    return s;
}

fn word(int n) -> string {
    let out: Vec<byte> = Vec::new();
    let i = 0;
    while i < n {
        out.push("a" as byte);
        i += 1;
    }
    return match from_bytes(out) {
        Result::Ok(s) => s,
        Result::Err(_) => "",
    };
}

fn broken() -> string {
    let out: Vec<byte> = Vec::new();
    out.push(255 as byte);
    return match from_bytes(out) {
        Result::Ok(s) => s,
        Result::Err(_) => "bad",
    };
}

fn commas(string s) -> int {
    let b = to_bytes(s);
    let n = 0;
    let i = 0;
    while i < b.len() {
        if b[i] == "," {
            n += 1;
        }
        i += 1;
    }
    return n;
}

fn maybe(int i) -> Option<string> {
    if i % 2 == 0 {
        return Option::Some(word(i));
    }
    return Option::None;
}

fn rebuild(int rounds) -> int {
    let list = build(10);
    let r = 0;
    let sum = 0;
    while r < rounds {
        list = build(100);
        collect();
        sum += total(list);
        r += 1;
    }
    return sum + total(list);
}

fn lengths(int n) -> int {
    let sum = 0;
    let i = 0;
    while i < n {
        sum += match maybe(i) {
            Option::Some(s) => s.len(),
            Option::None => 0,
        };
        i += 1;
    }
    collect();
    return sum;
}

test("a list rebuilt into the same local keeps the new list") {
    assert!(rebuild(20) == 21 * 4950);
}

test("a string built from a buffer survives collections") {
    let s = word(5);
    collect();
    assert!(s == "aaaaa");
    assert!(broken() == "bad");
}

test("an indexed byte copy reads the string") {
    assert!(commas("a,b,,c") == 3);
    assert!(commas("") == 0);
}

test("a matched string is read before it is freed") {
    assert!(lengths(50) == 600);
}
