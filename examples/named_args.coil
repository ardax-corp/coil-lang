// Named arguments, alone or after positional ones.
//
// @output Ada36Grace40

use io::sync::print;
use string::format;

fn greet(string name, int age) {
    let _ = print(format("%s", name));
    let _ = print(format("%i", age));
}

fn main() {
    greet(name: "Ada", age: 36);
    greet("Grace", age: 40);
}
