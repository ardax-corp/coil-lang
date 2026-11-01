// `io::attach` and `Stream.park` are compiler-known `io` items: importing
// them, and calling `park` on a stream, type-checks without any grant.
use tests::assert;
use io::{stdout, attach, park};

fn parks() {
    let s = stdout();
    let _ = s.park();
}

test("attach and park resolve") {
    let _ = parks;
    assert!(true);
}
