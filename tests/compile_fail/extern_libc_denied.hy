// Expected: compile failure — `extern "c"` (a libc alias) is always denied.
extern "c" {
    fn strlen(string s) -> int;
}

fn main() {
    let _ = strlen("hello");
}
