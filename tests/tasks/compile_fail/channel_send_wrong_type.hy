// Expected: E0102 — a `Channel<int>` only takes `int` values.
use task::Channel;

fn main() {
    let ch: Channel<int> = task::channel(1);
    let _ = ch.send("text");
}
