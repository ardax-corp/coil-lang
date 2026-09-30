// Edge cases for length-prefixed frames: empty body, truncate at 5.
use protocol::{encode_frame, frame_len, payload_eq};

test("empty frame") {
    let empty: Vec<byte> = Vec::new();
    let eframe = encode_frame(empty);
    assert(frame_len(eframe) == 0, "empty len")?;
    assert(payload_eq(eframe, empty) == 1, "empty roundtrip")?;
}

test("truncate payload at five bytes") {
    let long = Vec::from(
        [1 as byte, 2 as byte, 3 as byte, 4 as byte, 5 as byte, 6 as byte, 7 as byte],
    );
    let capped = encode_frame(long);
    assert(frame_len(capped) == 5, "cap at 5")?;
    let expect = Vec::from([1 as byte, 2 as byte, 3 as byte, 4 as byte, 5 as byte]);
    assert(payload_eq(capped, expect) == 1, "truncated payload")?;
}

test("one byte frame") {
    let short = Vec::from([9 as byte]);
    let one = encode_frame(short);
    assert(frame_len(one) == 1, "len 1")?;
    assert(payload_eq(one, short) == 1, "one byte")?;
}
