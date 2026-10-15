// Expected: E0413 — spawning a thread is the `thread` effect.
use thread::spawn;

pure fn go() {
    let h = spawn(fn () => 1);
}

fn main() {
    go();
}
