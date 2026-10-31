// `clock::mono_nanos` never goes backwards, `clock::sleep_ms` waits at least
// the time asked, and `clock::wall_nanos` is a Unix time in nanoseconds.
use tests::assert;
use clock::{mono_nanos, sleep_ms, wall_nanos};

test("mono_nanos never decreases") {
    let last = mono_nanos();
    let i = 0;
    while i < 1000 {
        let now = mono_nanos();
        assert!(now >= last);
        last = now;
        i += 1;
    }
}

test("sleep_ms waits at least that long") {
    let t0 = mono_nanos();
    sleep_ms(20);
    assert!(mono_nanos() - t0 >= 20 * 1000 * 1000);
}

test("sleep_ms of zero returns") {
    let t0 = mono_nanos();
    sleep_ms(0);
    assert!(mono_nanos() >= t0);
}

test("wall_nanos is after 2020 in nanoseconds") {
    let w = wall_nanos();
    // 2020-01-01T00:00:00Z
    assert!(w > 1577836800 * 1000 * 1000 * 1000);
}
