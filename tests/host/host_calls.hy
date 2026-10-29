// Host natives called from bodies lowered straight from HIR to MIR: a stream
// is a heap reference, and a host returning a boxed Result is unboxed into the
// `[payload, tag]` pair the body matches on.
use tests::assert;
use io::open;
use io::close;
use io::write_from;
use io::fs::remove_file;
use io::file::read_text;
use string::to_bytes;

fn put_all(Stream s, Vec<byte> buf) -> int {
    let offset = 0;
    let calls = 0;
    while offset < buf.len() {
        match write_from(s, buf, offset) {
            Result::Ok(n) => {
                offset = offset + n;
            },
            Result::Err(_) => {
                return -1;
            },
        }
        calls = calls + 1;
    }
    return calls;
}

test("write_from result unboxed in direct MIR") {
    let path = "coil_lang_mir_direct_host.txt";
    let s = open(path, "w")?;
    let calls = put_all(s, to_bytes("hello direct"));
    assert!(calls >= 1);
    close(s)?;
    assert!(read_text(path)? == "hello direct");
    remove_file(path)?;
}
