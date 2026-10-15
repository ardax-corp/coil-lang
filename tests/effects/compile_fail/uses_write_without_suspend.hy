// Expected: E0413 — `io::sync::write_all` may park until the stream is ready,
// so it needs `suspend` as well as `write`.
use io::stdout;
use io::sync::write_all;
use string::to_bytes;

fn say(string s) uses {write} {
    write_all(stdout(), to_bytes(s));
}

fn main() {
    say("x");
}
