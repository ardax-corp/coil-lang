// Generic type aliases expand at typecheck time.
//
// @output 7

use io::sync::print;
use string::format;

type Pair<T> = (T, T);

fn main() {
    let p: Pair<int> = (3, 4);
    let _ = print(format("%i", p[0] + p[1]));
}
