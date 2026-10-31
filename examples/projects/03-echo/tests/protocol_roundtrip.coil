// Pure unit test: protocol encode/decode (no sockets).
use tests::assert;
use protocol::{encode_frame, frame_len, payload_eq};

test("encode frame length") {
    let body = Vec::from([65 as byte, 66 as byte]);
    let frame = encode_frame(body);
    assert!(frame_len(frame) == 2, "len");
}

test("payload roundtrip and mismatch") {
    let body = Vec::from([65 as byte, 66 as byte]);
    let frame = encode_frame(body);
    assert!(payload_eq(frame, body) == 1, "roundtrip");
    let other = Vec::from([65 as byte, 67 as byte]);
    assert!(payload_eq(frame, other) == 0, "mismatch");
}
