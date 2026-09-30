use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

attr log<T>(fn(... args) -> T target, string message, ... args) -> T {
    write_all(stdout(), to_bytes(format("%s", message)));
    return target(...args);
}

attr measure<T>(fn(... args) -> T target, string metric, ... args) -> T {
    write_all(stdout(), to_bytes(format("%s", metric)));
    return target(...args);
}

#[log(message = "enter")]
#[measure(metric = "do_thing")]
fn do_thing(int x, string name) -> int {
    write_all(stdout(), to_bytes(format("%s", name)));
    return x;
}

fn main() {
    write_all(stdout(), to_bytes(format("%i", do_thing(42, "hi"))));
}
