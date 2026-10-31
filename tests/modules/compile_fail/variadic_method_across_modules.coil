// @expected E0101 — `Box::tally`'s `... xs` gets a copy per call shape only
// for calls in its own module, so another module cannot call it
// (coil-lang#1754).
use variadic_cross::Box;

fn main() {
    let n = new Box(1).tally(2, "b");
}
