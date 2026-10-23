// Pure unit test: save encode/decode roundtrip (no file IO).
use tests::assert;
use save::{decode_save, encode_save};
use world::{Player, Room};

test("encode decode hall without key") {
    let d = decode_save(encode_save(new Player(Room::Hall, false)))?;
    assert!(d.room == Room::Hall, "hall");
    assert!(!d.has_key, "no key");
}

test("encode decode library with key") {
    let d = decode_save(encode_save(new Player(Room::Library, true)))?;
    assert!(d.room == Room::Library, "library");
    assert!(d.has_key, "has key");
}

test("a short or unknown save is an error") {
    let short: Vec<byte> = [1];
    assert!(decode_save(short).is_err(), "too short");
    let bad: Vec<byte> = [7, 0];
    assert!(decode_save(bad).is_err(), "no room 7");
}
