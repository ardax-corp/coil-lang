// Output: AdaGrace:40
use io::{stdout, write};
use string::{format, to_bytes};

fn greet(string name) -> string {
    return name;
}

fn greet(string name, int age) -> string {
    return format("%s:%i", name, age);
}

fn main() {
    write(stdout(), to_bytes(format("%s", greet(name: "Ada"))));
    write(stdout(), to_bytes(format("%s", greet(name: "Grace", age: 40))));
}
