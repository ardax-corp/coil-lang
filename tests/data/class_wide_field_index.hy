// Fields past index 32 of a wide class read and write on direct MIR (#1498).
// Output: 5 40 7 90 124\n
use io::{stdout, write};
use string::{format, to_bytes};

class Wide {
    pub f0: int,
    pub f1: int,
    pub f2: int,
    pub f3: int,
    pub f4: int,
    pub f5: int,
    pub f6: int,
    pub f7: int,
    pub f8: int,
    pub f9: int,
    pub f10: int,
    pub f11: int,
    pub f12: int,
    pub f13: int,
    pub f14: int,
    pub f15: int,
    pub f16: int,
    pub f17: int,
    pub f18: int,
    pub f19: int,
    pub f20: int,
    pub f21: int,
    pub f22: int,
    pub f23: int,
    pub f24: int,
    pub f25: int,
    pub f26: int,
    pub f27: int,
    pub f28: int,
    pub f29: int,
    pub f30: int,
    pub f31: int,
    pub f32: int,
    pub f33: int,
    pub f34: int,
    pub f35: int,
    pub f36: int,
    pub f37: int,
    pub f38: int,
    pub f39: int,
    pub f40: int,
}

impl Wide {
    pub fn bump() {
        self.f40 = self.f40 + self.f33;
    }
}

fn main() {
    let w = new Wide(
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
        25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40,
    );
    w.f33 = 5;
    w.bump();
    let total = w.f33 + w.f40 + w.f40;
    let _ = write(
        stdout(),
        to_bytes(format("%i %i %i %i %i\n", w.f33, w.f40 - 5, w.f7, w.f40 * 2, total + 29)),
    );
}
