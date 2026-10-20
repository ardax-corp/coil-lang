// Expected: E0107 — C may write a `Bytes` (`uint8_t *`) argument, so a `readonly` buffer goes only to `ConstBytes` (#1558).
use ffi::declare;
use ffi::dload;
use ffi::invoke;
use ffi::types::Bytes;
use ffi::types::ConstBytes;
use ffi::types::Int;

fn main() {
    let lib = match dload("z") {
        Result::Ok(h) => h,
        Result::Err(e) => panic e.message,
    };
    let crc32 = match declare(lib, "crc32", (Int, ConstBytes, Int), Int) {
        Result::Ok(i) => i,
        Result::Err(e) => panic e.message,
    };
    let fill = match declare(lib, "fill", (Bytes, Int), Int) {
        Result::Ok(i) => i,
        Result::Err(e) => panic e.message,
    };
    let data: Vec<byte> = [1 as byte, 2 as byte];
    let view = readonly data;
    let _ = invoke(lib, crc32, (0, view, 2));
    let _ = invoke(lib, fill, (view, 2));
}
