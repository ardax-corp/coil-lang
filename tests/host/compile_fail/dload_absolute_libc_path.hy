// Expected: E0410 — an absolute path to libc is the libc alias, always denied.
use ffi::dload;

fn main() {
    let _ = dload("/lib/x86_64-linux-gnu/libc.so.6");
}
