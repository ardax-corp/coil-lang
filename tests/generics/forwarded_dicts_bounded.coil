// A shared generic body that forwards its own dictionary parameter to
// another bounded generic, from direct MIR: the dictionary stays live
// across the calls and allocations of a loop.
use tests::assert;
use string::format;

fn label_at<T: Show>(Vec<T> xs, int i) -> string {
    return format("<%v>", xs[i]);
}

fn labels<T: Show>(Vec<T> xs, int n) -> string {
    let out = "";
    let i = 0;
    while i < n {
        out = out + label_at(xs, i);
        i = i + 1;
    }
    return out;
}

test("ints through a forwarded dictionary") {
    let v: Vec<int> = Vec::new();
    for i in 0..4 {
        v.push(i * 3);
    }
    assert!(labels(v, 4) == "<0><3><6><9>");
}

test("strings through a forwarded dictionary") {
    let v: Vec<string> = Vec::new();
    v.push("a");
    v.push("bc");
    assert!(labels(v, 2) == "<a><bc>");
}
