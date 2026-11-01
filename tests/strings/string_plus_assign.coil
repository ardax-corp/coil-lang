// `s += t` on strings concatenates.
use tests::assert;
use io::{stdout, write};
use string::{format, to_bytes};

test("string += concatenates") {
    let s = "a";
    s += "b";
    s += "c";
    assert!(s == "abc");
    let _ = write(stdout(), to_bytes(format("")));
}
