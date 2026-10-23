// FFI: byte buffers handed to zlib. A 1 MiB `Vec<byte>` goes to `crc32`
// (`ConstBytes`) whole 200 times and for its first 4 KiB 20,000 times, and
// `uncompress` (`Bytes`) writes 4 KiB into a 1 MiB `Vec<byte>` 20,000 times:
// what the native touches is small next to what a copy would move.
//
// Run: coil --allow-dload z --dload-trusted z examples/perf/ffi_bytes.hy
use ffi::declare;
use ffi::dload;
use ffi::invoke;
use ffi::types::Bytes;
use ffi::types::ConstBytes;
use ffi::types::Int;
use ffi::types::Ptr;
use io::sync::println;
use string::format;

fn filled(int n, int step) -> Vec<byte> {
    let out: Vec<byte> = Vec::with_capacity(n);
    let i = 0;
    while i < n {
        out.push(((i * step + i / 256) % 251) as byte);
        i = i + 1;
    }
    return out;
}

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
    let big = 1048576;
    let small = 4096;
    let data = filled(big, 7);
    let crc = 0;
    let round = 0;
    while round < 200 {
        crc = match invoke(lib, crc32, (crc, data, big)) {
            Result::Ok(c) => c,
            Result::Err(e) => panic e.message,
        };
        round = round + 1;
    }
    round = 0;
    while round < 20000 {
        crc = match invoke(lib, crc32, (crc, data, small)) {
            Result::Ok(c) => c,
            Result::Err(e) => panic e.message,
        };
        round = round + 1;
    }
    let packed = filled(small + 512, 0);
    let packed_len = [small + 512];
    let _ = invoke(lib, compress, (packed, packed_len, filled(small, 3), small));
    let out = filled(big, 0);
    round = 0;
    while round < 20000 {
        let out_len = [small];
        let _ = invoke(lib, uncompress, (out, out_len, packed, packed_len[0]));
        round = round + 1;
    }
    let _ = println(format("crc=%i out[4095]=%i", crc, out[4095] as int));
}
