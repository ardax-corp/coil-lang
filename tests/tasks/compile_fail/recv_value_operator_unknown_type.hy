// @expected E0119 — `recv` on a bare `Receiver` never learns what the channel
// carries, so `n * n` has no operand type: a type error naming the operator,
// not a codegen failure (#1845).
use thread::{recv, send, Receiver, Sender};

fn worker((Receiver, Sender) ends) {
    while let Ok(n) = recv(ends[0]) {
        send(ends[1], n * n)?;
    }
}

fn main() {}
