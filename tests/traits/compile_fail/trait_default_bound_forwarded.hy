// Expected: E0801 — a trait default body's own bounded type parameter
// (`fn third<W: Show>`) has no dictionary in its frame yet, so passing it
// to a bounded generic is refused at compile time rather than failing at
// run time (#1471).
trait Pass<S> {
    fn third<W: Show>(S s, W w) -> string {
        return shout(w);
    }
}

fn shout<X: Show>(X x) -> string {
    return show(x) + "!";
}

impl Pass for int {
}

fn main() {
    let _ = 3.third(4);
}
