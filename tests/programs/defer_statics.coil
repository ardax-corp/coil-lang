// `defer` blocks run LIFO on function exit; `static let` and module `const` are shared across calls.
// @output body\n2\n1\nhits=2 v1\n
use io::sync::println;
use string::format;

static let hits = 0;

const VERSION = "v1";

fn work() -> int {
    defer {
        let _ = println("1");
    }
    defer {
        let _ = println("2");
    }
    hits += 1;
    let _ = println("body");
    return hits;
}

fn main() {
    let _ = work();
    hits += 1;
    let _ = println(format("hits=%i %s", hits, VERSION));
}
