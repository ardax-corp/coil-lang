// Expected: E0410 — `extern "c"` (a libc alias) is always denied.
extern "c" {
    fn strlen(string s) -> int;
}

fn main() {
    let _ = strlen("hello");
}
