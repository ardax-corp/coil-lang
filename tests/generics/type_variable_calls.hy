// Shared generic bodies pass a type variable's word on to calls: a closure
// call, a generic call and `push` on a `Vec<T>`. The words go on as they
// are, and allocating callees in between must not lose them.
use tests::assert;
use string::format;
use collections::vec::{chunks, concat, filter, map, partition, windows};

fn apply_to<T, U>(T x, T -> U f) -> U {
    return f(x);
}

fn twice<T>(T x, T -> T f) -> T {
    return apply_to(apply_to(x, f), f);
}

fn tag(string s) -> string {
    return format("<%v>", s);
}

test("map and filter over strings with allocating closures") {
    let words: Vec<string> = ["a", "bb", "ccc", "dddd"];
    let tagged = map(words, fn (string s) => format("%v!", s));
    assert!(tagged[3] == "dddd!");
    let long = filter(tagged, fn (string s) => len(format("%v%v", s, s)) > 6);
    assert!(len(long) == 2);
    assert!(long[0] == "ccc!");
    let sizes = map(words, fn (string s) => len(s));
    assert!(sizes[2] == 3);
}

test("push of a type variable word") {
    let a: Vec<string> = ["x", "y"];
    let b: Vec<string> = [format("%v", 1), format("%v", 2), format("%v", 3)];
    let both = concat(a, b);
    assert!(len(both) == 5);
    assert!(both[4] == "3");
    let cs = chunks(both, 2);
    assert!(len(cs) == 3);
    assert!(cs[2][0] == "3");
    let ws = windows(b, 2);
    assert!(ws[1][1] == "3");
    let parts = partition(both, fn (string s) => s == "x" || s == "2");
    assert!(len(parts.matched()) == 2);
}

test("generic calls pass the word on") {
    assert!(twice("a", tag) == "<<a>>");
    assert!(
        apply_to(
            Option::Some("q"),
            fn (Option<string> o) => match o {
                Option::Some(s) => s == "q",
                Option::None => false,
            },
        ),
    );
}
