// A class field of a scalar enum type holds the enum's backing word (an
// int, float, bool or string), which direct MIR reads, writes and makes.
use tests::assert;

#[derive(Eq, Ord)]
enum Level {
    Low = 0,
    Mid = 1,
    High = 2,
}

#[repr(string)]
#[derive(Eq)]
enum Mode {
    Read = "r",
    Write = "w",
}

#[repr(float)]
enum Scale {
    Half = 0.5,
    Double = 2.0,
}

#[repr(bool)]
enum Flag {
    Off = false,
    On = true,
}

class Gate {
    min: Level,
    mode: Mode,
    scale: Scale,
    flag: Flag,
}

impl Gate {
    pub static fn new(Level min, Mode mode) -> Gate {
        return new Gate(min, mode, Scale::Half, Flag::Off);
    }

    pub fn pass(Level level) -> bool {
        return level >= self.min;
    }

    pub fn writes() -> bool {
        return self.mode == Mode::Write;
    }

    pub fn raise(Level to) {
        self.min = to;
        self.mode = Mode::Write;
        self.scale = Scale::Double;
        self.flag = Flag::On;
    }

    pub fn factor() -> float {
        return match self.scale {
            Scale::Half => 0.5,
            Scale::Double => 2.0,
        };
    }

    pub fn on() -> bool {
        return match self.flag {
            Flag::Off => false,
            Flag::On => true,
        };
    }
}

test("scalar enum fields") {
    let g = Gate::new(Level::Mid, Mode::Read);
    assert!(g.pass(Level::High));
    assert!(!g.pass(Level::Low));
    assert!(!g.writes());
    assert!(g.factor() == 0.5);
    assert!(!g.on());
    g.raise(Level::High);
    assert!(!g.pass(Level::Mid));
    assert!(g.writes());
    assert!(g.factor() == 2.0);
    assert!(g.on());
}
