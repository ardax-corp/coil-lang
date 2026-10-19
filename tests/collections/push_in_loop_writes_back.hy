// #1295: `v.push(x)` in a loop defines the pushed array back into `v`, so
// the loop's φ for `v` takes the push on its latch instead of merging only
// with itself. These loops read `v` after pushing in the same iteration.
use tests::assert;

test("a while whose condition reads the pushed vector") {
    let v: Vec<int> = [1];
    while v.len() < 5 {
        v.push(v[v.len() - 1] * 2);
    }
    assert!(v[4] == 16);
    assert!(v.len() == 5);
}

test("a push on one branch of a loop body") {
    let v: Vec<int> = [];
    let i = 0;
    while i < 10 {
        if i % 3 == 0 {
            v.push(i);
        }
        i += 1;
    }
    assert!(v.len() == 4);
    assert!(v[3] == 9);
}

test("pushes in nested loops") {
    let v: Vec<int> = [];
    for a in 0..3 {
        for b in 0..a {
            v.push(a * 10 + b);
        }
        v.push(v.len());
    }
    assert!(v.len() == 6);
    assert!(v[5] == 5);
}

test("a vector of strings grown until a length") {
    let words: Vec<string> = ["a"];
    while words.len() < 4 {
        words.push(words[words.len() - 1] + "a");
    }
    assert!(words[3] == "aaaa");
}
