// A rest parameter `T... xs` packs every trailing argument into one Vec<T>:
// none, one or many, after fixed parameters, and from spreads mixed with
// plain arguments.
use tests::assert;

fn sum(int... xs) -> int {
    let t = 0;
    for x in xs {
        t = t + x;
    }
    return t;
}

fn count(string... xs) -> int {
    return len(xs);
}

fn joined(string sep, string... parts) -> string {
    let out = "";
    let i = 0;
    while i < len(parts) {
        if i > 0 {
            out = out + sep;
        }
        out = out + parts[i];
        i = i + 1;
    }
    return out;
}

test("zero, one and many values") {
    assert!(sum() == 0);
    assert!(sum(7) == 7);
    assert!(sum(1, 2, 3, 4, 5) == 15);
    assert!(sum(-1, 1) == 0);
}

test("string rest") {
    assert!(count() == 0);
    assert!(count("a") == 1);
    assert!(count("a", "b", "c") == 3);
}

test("rest after a fixed parameter keeps order") {
    assert!(joined("-") == "");
    assert!(joined("-", "a") == "a");
    assert!(joined("-", "a", "b", "c") == "a-b-c");
    assert!(joined(sep: ",") == "");
}

test("an array spread between plain arguments") {
    assert!(sum(1, ...[2, 3], 4) == 10);
}

test("a spread before a plain argument") {
    assert!(count(...["x", "y"], "z") == 3);
}

test("a tuple spread into the rest after a fixed argument") {
    assert!(joined("+", ...("a", "b")) == "a+b");
}

test("an array spread after fixed and plain rest arguments") {
    assert!(joined("/", "a", ...["b", "c"]) == "a/b/c");
}
