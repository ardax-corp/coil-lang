// @expected E0413 — writing to a stream is the `write` effect.
use io::{stdout, write};
use string::to_bytes;

pure fn say(string s) {
    write(stdout(), to_bytes(s));
}

fn main() {
    say("x");
}
