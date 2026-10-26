// Objects that never leave a block are freed where the block ends, and call
// temporaries right after the call (docs/internals/escape-memory.md, step 2).
// Whatever is stored, returned or shared must stay alive across collections.
use tests::assert;
use gc::collect;
use string::to_bytes;

class Tree {
    pub left: Option<Tree>,
    pub right: Option<Tree>,
}

fn make(int depth) -> Tree {
    if depth == 0 {
        return new Tree(Option::None, Option::None);
    }
    return new Tree(Option::Some(make(depth - 1)), Option::Some(make(depth - 1)));
}

fn check(Tree t) -> int {
    let n = 1;
    if let Option::Some(l) = t.left {
        n += check(l);
    }
    if let Option::Some(r) = t.right {
        n += check(r);
    }
    return n;
}

fn sum(Vec<int> v) -> int {
    let s = 0;
    for x in v {
        s += x;
    }
    return s;
}

test("a tree checked right after it is built is freed whole") {
    let total = 0;
    let i = 0;
    while i < 200 {
        total += check(make(6));
        i += 1;
    }
    assert!(total == 200 * 127);
}

test("block locals are freed at the end and kept values survive") {
    let kept: Vec<Tree> = Vec::new();
    let i = 0;
    while i < 100 {
        let scratch = make(4);
        let local: Vec<int> = Vec::new();
        local.push(check(scratch));
        local.push(i);
        if i % 10 == 0 {
            kept.push(make(3));
        }
        assert!(sum(local) == 31 + i);
        i += 1;
    }
    collect();
    let n = 0;
    for t in kept {
        n += check(t);
    }
    assert!(n == 10 * 15);
}

fn longest(Vec<string> words) -> string {
    let best = "";
    for w in words {
        let copy = to_bytes(w);
        if copy.len() > to_bytes(best).len() {
            best = w;
        }
    }
    return best;
}

test("a returned value that came through a freed local's block stays alive") {
    let words: Vec<string> = Vec::new();
    words.push("ab");
    words.push("abcd");
    words.push("abc");
    let w = longest(words);
    collect();
    assert!(w == "abcd");
}
