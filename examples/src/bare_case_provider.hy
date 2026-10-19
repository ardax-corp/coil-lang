// Provider for tests/patterns/bare_imported_case_in_pure_fn.hy: an enum whose
// unit cases the test matches bare (`On`, `Off`) after importing it.

enum Switch {
    On,
    Off,
}

fn mode(int n) -> Switch {
    if n <= 1 {
        return Switch::Off;
    }
    return Switch::On;
}
