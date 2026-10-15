// Matching a scalar-backed enum (string, float or bool backing) selects
// by case; `default` covers the cases not listed.
use tests::assert;

#[repr(string)]
enum Mode {
    Fast = "fast",
    Slow = "slow",
    Off = "off",
}

#[repr(bool)]
enum Switch {
    On = true,
    Down = false,
}

#[repr(float)]
enum Ratio {
    Half = 0.5,
    Whole = 1.0,
}

fn speed(Mode m) -> int {
    return match m {
        Mode::Fast => 3,
        Mode::Slow => 1,
        default => 0,
    };
}

fn moving(Mode m) -> bool {
    return match m {
        Mode::Off => false,
        default => true,
    };
}

fn flip(Switch s) -> Switch {
    return match s {
        Switch::On => Switch::Down,
        Switch::Down => Switch::On,
    };
}

fn scale(Ratio r, int n) -> int {
    return match r {
        Ratio::Half => n / 2,
        Ratio::Whole => n,
    };
}

test("string-backed cases") {
    assert!(speed(Mode::Fast) == 3);
    assert!(speed(Mode::Slow) == 1);
    assert!(speed(Mode::Off) == 0);
    assert!(moving(Mode::Slow));
    assert!(!moving(Mode::Off));
}

test("bool-backed cases") {
    assert!(flip(Switch::On) == Switch::Down);
    assert!(flip(flip(Switch::On)) == Switch::On);
}

test("float-backed cases") {
    assert!(scale(Ratio::Half, 10) == 5);
    assert!(scale(Ratio::Whole, 10) == 10);
}
