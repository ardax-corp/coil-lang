// Expected: E0413 — `uses {suspend}` does not cover the `write` effect of
// `io::sync::write_all`.
use io::stdout;
use io::sync::write_all;
use string::to_bytes;

fn say(string s) uses {suspend} {
    write_all(stdout(), to_bytes(s));
}
