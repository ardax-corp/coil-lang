// @expected E0200 — `ThreadError` is a built-in error enum; an enum cannot
// take the name, imported or not (#1701).
enum ThreadError {
    Busy,
    Done,
}

fn busy(ThreadError e) -> bool {
    return match e {
        ThreadError::Busy => true,
        ThreadError::Done => false,
    };
}

fn main() {
    let _ = busy(ThreadError::Busy);
}
