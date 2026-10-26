// @expected E0119 — a function value is not sendable, so `thread::send`
// cannot put it on a channel (the runtime used to report `NotSendable`).
use thread::{channel, send};

fn main() {
    let (tx, rx) = match channel() {
        Ok(pair) => pair,
        Err(_) => panic "channel failed",
    };
    let _ = send(tx, fn (int n) => n + 1);
}
