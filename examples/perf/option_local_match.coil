// CPU: in-frame Option construct + match (no heap ObjEnum when it does not escape).
use io::sync::print;
use string::format;

fn main() {
    let acc = 0;
    let i = 0;
    while i < 20000 {
        acc = acc + match Option::Some(i % 7) {
            Option::Some(x) => x,
            Option::None => 0,
        };
        acc = acc + match Option::None {
            Option::Some(_) => 1,
            Option::None => 0,
        };
        i = i + 1;
    }
    let _ = print(format("%i", acc));
}
