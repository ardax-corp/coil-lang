// A range value read by a call nested in another call's arguments stages
// into a temp, with the calls before it, so it runs with nothing below it.
use string::format;
test("range to_vec inside format") {
    let a = (0..5).to_vec();
    let r = 0..=3;
    let lo: byte = 5;
    let hi: byte = 6;
    let s = format("%i,%i,%i,%i,%i", a.len(), r.to_vec().len(), (10..0).to_vec().len(), (lo..=hi).to_vec().len(), (1.0..4.0).to_vec().len());
    assert(s == "5,4,0,2,3", s)?;
}
