// Expected: E0119 — the old runtime decorator form `attr f(fn target, …)` is gone: an
// attribute macro takes a `FnDecl` or `TypeDecl` first.
attr log<T>(fn(... args) -> T target, string message, ... args) -> T {
    return target(...args);
}

fn main() {}
