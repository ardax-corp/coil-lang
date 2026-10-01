// An `extern` block in an imported module (used by examples/ffi_mod_entry.hy).
extern "sum" {
    fn sum(int a, int b) -> int;
}

fn run_twice() -> int {
    let a = sum(1, 2);
    // A heap allocation between the calls must not disturb the binding.
    let v = Vec::new();
    v.push("x");
    let b = sum(3, 4);
    return a + b;
}
