// `region { … }` frees what it made when it ends (escape-memory step 5).
// Its value may be a plain value or an object made before it; `return`,
// `?`, `break` and `continue` leave it on the way out; a region nests and
// `region` stays usable as a name.
use tests::assert;
use gc::collect;
use string::format;

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
    let n = 0;
    let cur = head;
    while let Option::Some(node) = cur {
        n += node.v;
        cur = node.next;
    }
    return n;
}

fn parse(int n) -> Result<int, string> {
    if n < 0 {
        return Result::Err(format("negative %i", n));
    }
    return Result::Ok(n);
}

// `?` leaves the region with an error made inside it.
fn checked(int n) -> Result<int, string> {
    let v = region {
        let list = build(10);
        parse(n)? + total(list)
    };
    return Result::Ok(v);
}

// `return` from inside a region hands out an object made inside it.
fn label(int n) -> string {
    region {
        let s = format("item %i", n);
        if n > 2 {
            return s;
        }
    }
    return "small";
}

fn main() {
    let kept = build(5);
    let sum = 0;
    let round = 0;
    while round < 300 {
        sum += region {
            let list = build(100 + round);
            total(list) - total(build(100 + round)) + round
        };
        region {
            let skip = build(3);
            if total(skip) == 3 && round % 2 == 0 {
                round += 1;
                continue;
            }
            region {
                let inner = format("%i", round);
                if inner.len() > 5 {
                    break;
                }
            }
        }
        round += 1;
    }
    collect();
    assert!(sum == 44850);
    assert!(total(kept) == 10);
    let same = region {
        kept
    };
    assert!(total(same) == 10);
    let err = match checked(-4) {
        Result::Err(e) => e,
        Result::Ok(_) => "",
    };
    assert!(err == "negative -4");
    let ok = match checked(7) {
        Result::Ok(v) => v,
        Result::Err(_) => 0,
    };
    assert!(ok == 52);
    collect();
    assert!(label(5) == "item 5");
    assert!(label(1) == "small");
    let region = 2;
    assert!(region + 1 == 3);
}
