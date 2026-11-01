// Operator precedence, `**` associativity, compound assignment, `++` / `--`, assignment as a value, shadowing and a byte cast.
// @output 7 3 20 512 10 -3 true 1 3 5 1 0 hi there 97\n
use io::sync::println;
use string::format;

fn main() {
    let a = 1 + 2 * 3;
    let b = (1 + 2) * 3 / 3;
    let c = 2 ** 2 * 5;
    let d = 2 ** 3 ** 2;
    let mut_x = 5;
    mut_x += 5;
    let neg = -3;
    let cmp = 1 < 2 && !(3 == 4) || false;
    let x = 1;
    let y = x++;
    let z = ++x;
    let s = 0;
    let s2 = s = 5;
    let shadow = 1;
    {
        let shadow = 0;
        let _ = shadow;
    }
    let t = "hi" + " " + "there";
    let ch = "a" as byte;
    let _ = println(
        format(
            "%i %i %i %i %i %i %z %i %i %i %i %i %s %i",
            a,
            b,
            c,
            d,
            mut_x,
            neg,
            cmp,
            y,
            z,
            s2,
            shadow,
            1 - 1,
            t,
            ch,
        ),
    );
}
