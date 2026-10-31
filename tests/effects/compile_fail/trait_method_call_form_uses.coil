// @expected E0413 — `a.emit()` on a bound receiver has the effects its trait
// method declares (`uses {write}`), as `emit(a)` does.
trait Sink<S> {
    fn emit(S self) -> int uses {write} {}
}

class Out {
    pub n: int,
}

impl Sink for Out {
    fn emit(Out self) -> int {
        return self.n;
    }
}

pure fn send<T: Sink>(T a) -> int {
    return a.emit();
}

fn main() {
    send(new Out(1));
}
