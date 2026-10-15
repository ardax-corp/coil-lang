// Expected: E0410 — first-party stems (`time`, `crypto`, `tls`, `regex`)
// still need `--allow-dload`, even as absolute paths.
use ffi::dload;

fn main() {
    let _ = dload("/coil-dload-missing/libtime.so");
    let _ = dload("/coil-dload-missing/libcrypto.so");
    let _ = dload("/coil-dload-missing/libtls.so");
    let _ = dload("/coil-dload-missing/libregex.so");
}
