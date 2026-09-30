use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn greet(string name, int age) {
    write_all(stdout(), to_bytes(format("%s", name)));
    write_all(stdout(), to_bytes(format("%i", age)));
}

fn main() {
    greet(name: "Ada", age: 36);
    greet("Grace", age: 40);
}
