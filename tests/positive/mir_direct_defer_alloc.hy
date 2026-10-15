// A `defer` whose body allocates lowers to direct MIR: its cleanup pad
// (the copy that runs while a panic unwinds) maps its heap words for the
// collector.
use string::format;
use tests::assert;

static let LOG = "";

fn note(string s) {
    LOG = LOG + s;
}

fn work(int k) -> int {
    defer use (k) {
        note(format("[%i]", k));
    }
    if k > 100 {
        panic "too big";
    }
    return k * 2;
}

fn twice(string tag) -> int {
    defer use (tag) {
        note(format("<%s>", tag));
    }
    return work(1) + work(2);
}

test("an allocating defer runs on the way out") {
    assert!(twice("t") == 6);
    assert!(LOG == "[1][2]<t>");
}
