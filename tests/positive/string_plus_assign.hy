// `s += "b"` on a string must concatenate (FORMAT), not integer-ADD the
// pointers. The target's type comes from the checker sidecar, not a
// name-keyed lookup that another `s` in a linked module can shadow.
use io::{stdout, write};
use string::{format, to_bytes};

test("string += concatenates") {
    let s = "a";
    s += "b";
    s += "c";
    assert(s == "abc")?;
    let _ = write(stdout(), to_bytes(format("")));
}
