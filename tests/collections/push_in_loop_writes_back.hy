// #1295: `v.push(x)` in a loop defines the pushed array back into `v`, so
// the loop's φ for `v` takes the push on its latch instead of merging only
// with itself. These loops read `v` after pushing in the same iteration.
use tests::assert;

test("a while whose condition reads the pushed vector") {
    let v: Vec<int> = [1];
    while len(v) < 5 {
        v.push(v[len(v) - 1] * 2);
    }
    assert!(v[4] == 16);
    assert!(len(v) == 5);
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
    assert!(len(v) == 4);
    assert!(v[3] == 9);
}

test("pushes in nested loops") {
    let v: Vec<int> = [];
    for a in 0..3 {
        for b in 0..a {
            v.push(a * 10 + b);
        }
        v.push(len(v));
    }
    assert!(len(v) == 6);
    assert!(v[5] == 5);
}

test("a vector of strings grown until a length") {
    let words: Vec<string> = ["a"];
    while len(words) < 4 {
        words.push(words[len(words) - 1] + "a");
    }
    assert!(words[3] == "aaaa");
}
