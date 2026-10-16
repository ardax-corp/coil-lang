// An exhaustive match over a `#[repr(string)]` enum needs no `default`
// arm (#1163).
use tests::assert;

#[repr(string)]
enum Mode {
    Fast = "fast",
    Slow = "slow",
    Off = "off",
}

fn speed(Mode m) -> int {
    return match m {
        Mode::Fast => 3,
        Mode::Slow => 1,
        Mode::Off => 0,
    };
}

fn label(Mode m) -> string {
    let out = "";
    match m {
        Mode::Fast => {
            out = "f";
        },
        Mode::Slow => {
            out = "s";
        },
        Mode::Off => {
            out = "o";
        },
    }
    return out;
}

test("an exhaustive string-backed match picks each case") {
    assert!(speed(Mode::Fast) == 3);
    assert!(speed(Mode::Slow) == 1);
    assert!(speed(Mode::Off) == 0);
}

test("an exhaustive string-backed match as a statement") {
    assert!(label(Mode::Fast) == "f");
    assert!(label(Mode::Slow) == "s");
    assert!(label(Mode::Off) == "o");
}
