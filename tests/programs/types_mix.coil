// Type aliases, float / int division, casts, `%`, a parametric alias and `typeof` on several values.
// @output 3 2.5 3 true 255 1 7 0 -3 12 int float string (int, string) [int; 3]\n
use io::sync::println;
use string::format;

type Id = int;

type Pair<T> = (T, T);

fn id(Id x) -> Id {
    return x;
}

fn main() {
    let f = 5.0 / 2.0;
    let fi = 3.9 as int;
    let bf = (7 % 2) as bool;
    let n = 257;
    let b = 255 as byte;
    let nb = n as byte;
    let i = 7 / 1;
    let z = -7 / 8;
    let m = -7 % 4; // sign follows dividend?
    let p: Pair<int> = (5, 7);
    let s = p[0] + p[1];
    let _ = println(
        format(
            "%i %f %i %z %i %i %i %i %i %i %s %s %s %s %s",
            id(3),
            f,
            fi,
            bf,
            b,
            nb,
            i,
            z,
            m,
            s,
            typeof 1,
            typeof 1.0,
            typeof "",
            typeof (1, ""),
            typeof [1, 2, 3],
        ),
    );
}
