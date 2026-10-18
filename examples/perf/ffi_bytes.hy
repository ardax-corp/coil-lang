// FFI: byte buffers handed to zlib. `crc32` reads a 64 KiB `Vec<byte>`
// (`ConstBytes`) and `uncompress` writes one (`Bytes`), 20,000 times each.
//
// Run: coil --allow-dload z --dload-trusted z examples/perf/ffi_bytes.hy
use ffi::declare;
use ffi::dload;
use ffi::invoke;
use ffi::types::Bytes;
use ffi::types::ConstBytes;
use ffi::types::Int;
use ffi::types::Ptr;
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    let lib = match dload("z") {
        Result::Ok(h) => h,
        Result::Err(e) => panic e.message,
    };
    // uLong crc32(uLong crc, const Bytef *buf, uInt len)
    let crc32 = match declare(lib, "crc32", (Int, ConstBytes, Int), Int) {
        Result::Ok(i) => i,
        Result::Err(e) => panic e.message,
    };
    // int compress(Bytef *dest, uLongf *destLen, const Bytef *src, uLong srcLen)
    let compress = match declare(lib, "compress", (Bytes, Ptr, ConstBytes, Int), Int) {
        Result::Ok(i) => i,
        Result::Err(e) => panic e.message,
    };
    // int uncompress(Bytef *dest, uLongf *destLen, const Bytef *src, uLong srcLen)
    let uncompress = match declare(lib, "uncompress", (Bytes, Ptr, ConstBytes, Int), Int) {
        Result::Ok(i) => i,
        Result::Err(e) => panic e.message,
    };
    let n = 65536;
    let data: Vec<byte> = Vec::with_capacity(n);
    let i = 0;
    while i < n {
        data.push(((i * 7 + i / 256) % 251) as byte);
        i = i + 1;
    }
    let packed: Vec<byte> = Vec::with_capacity(n + 1024);
    i = 0;
    while i < n + 1024 {
        packed.push(0 as byte);
        i = i + 1;
    }
    let packed_len = [n + 1024];
    let _ = invoke(lib, compress, (packed, packed_len, data, n));
    let out: Vec<byte> = Vec::with_capacity(n);
    i = 0;
    while i < n {
        out.push(0 as byte);
        i = i + 1;
    }
    let crc = 0;
    let round = 0;
    while round < 20000 {
        crc = match invoke(lib, crc32, (crc, data, n)) {
            Result::Ok(c) => c,
            Result::Err(e) => panic e.message,
        };
        let out_len = [n];
        let _ = invoke(lib, uncompress, (out, out_len, packed, packed_len[0]));
        round = round + 1;
    }
    write_all(stdout(), to_bytes(format("crc=%i out[4097]=%i\n", crc, out[4097] as int)));
}
