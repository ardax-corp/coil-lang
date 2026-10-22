// Compound assignment, `++` / `--`, and `**`.
//
// @output 801125428falsetrue3

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    let x = 5;
    x += 3;
    let _ = write_all(stdout(), to_bytes(format("%i", x)));

    let y = 0;
    let _ = write_all(stdout(), to_bytes(format("%i", y++)));
    let _ = write_all(stdout(), to_bytes(format("%i", y)));

    let z = 0;
    let _ = write_all(stdout(), to_bytes(format("%i", ++z)));

    let arr = [10, 20, 30];
    arr[1] += 5;
    let _ = write_all(stdout(), to_bytes(format("%i", arr[1])));

    let d = { val: 1 };
    d.val += 41;
    let _ = write_all(stdout(), to_bytes(format("%i", d.val)));

    let _ = write_all(stdout(), to_bytes(format("%i", 2 ** 3)));
    let _ = write_all(stdout(), to_bytes(format("%z", true && false)));
    let _ = write_all(stdout(), to_bytes(format("%z", 5 != 4)));
    let _ = write_all(stdout(), to_bytes(format("%i", 7 & 3)));
}
