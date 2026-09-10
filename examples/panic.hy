// Language-level panic: aborts the process (exit code 1).
// Contrast `raise` in examples/raise_try.hy — catchable Result.Err, not abort.
fn main() {
    panic "boom";
}
