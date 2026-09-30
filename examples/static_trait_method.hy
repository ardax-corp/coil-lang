// examples/static_trait_method.hy — `static fn` in a trait: a constructor
// chosen by its owner (`Point::from_val(v)`) or by the expected type of a
// generic call (`let p: Point = decode(v)`).
//
// Output: 4,8,0

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

class Val { pub i: int, }
class Point { pub x: int, }

trait FromVal<T> {
    static fn from_val(Val v) -> T {}
}

impl FromVal for Point {
    pub static fn from_val(Val v) -> Point { return new Point(v.i); }
}

impl FromVal for int {
    pub static fn from_val(Val v) -> int { return v.i * 2; }
}

#[derive(Default)]
class Config { pub port: int, pub name: string, }

fn decode<T: FromVal>(Val v) -> T {
    return T::from_val(v);
}

fn main() {
    let p = Point::from_val(new Val(4));
    let n: int = decode(new Val(4));
    let cfg = Config::default();
    write_all(stdout(), to_bytes(format("%i,%i,%i", p.x, n, cfg.port)));
}
