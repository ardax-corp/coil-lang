// A generator left suspended with a `defer` armed is closed once nothing
// reaches it: by the collector, or at teardown for one still live when
// `main` returns. A panic unwinding through a `for` loop's body leaves its
// generator to the collector too.
// @output abandoning\n  defer abandoned\nafter collect\n  defer panicked through\nafter second collect\nlast\n  defer live at exit\n
use io::sync::println;
use gc::collect;
use task::{scope, Scope};

gen fn lines(string tag) {
    defer use (tag) {
        let _ = println("  defer " + tag);
    }
    yield 1;
    yield 2;
    return 0;
}

fn abandon() {
    let h = lines("abandoned");
    let _ = println("abandoning");
    let _ = resume h;
}

fn panics() -> int {
    for v in lines("panicked through") {
        panic "body";
    }
    return 0;
}

fn main() {
    abandon();
    collect();
    let _ = println("after collect");
    let _ = scope(fn (Scope s) {
        let _ = s.spawn(fn () => panics());
        0
    });
    collect();
    let _ = println("after second collect");
    // Closed at teardown, or by a collection once `h` is dead: after
    // this line either way.
    let _ = println("last");
    let h = lines("live at exit");
    let _ = resume h;
}
