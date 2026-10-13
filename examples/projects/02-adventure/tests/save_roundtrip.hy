// Pure unit test: save encode/decode roundtrip (no file IO).
use tests::assert;
use save::{decode_save, encode_save};

test("encode decode hall without key") {
    let p0 = encode_save(0, 0);
    let d0 = decode_save(p0);
    assert!(d0.room == 0, "room 0");
    assert!(d0.has_key == 0, "no key");
}

test("encode decode library with key") {
    let p1 = encode_save(1, 1);
    let d1 = decode_save(p1);
    assert!(d1.room == 1, "library");
    assert!(d1.has_key == 1, "has key");
}

test("encode decode garden without key") {
    let p2 = encode_save(2, 0);
    let d2 = decode_save(p2);
    assert!(d2.room == 2, "garden");
    assert!(d2.has_key == 0, "garden no key");
}
