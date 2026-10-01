// FizzBuzz-style branching on `%`.
//
// Output: FIZBUZFIZFIZBUZFIZFIZBUZ

use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn fizbuz(int n) {
    if (n % 3) == 0 {
        write_all(stdout(), to_bytes("FIZ"));
    }
    if (n % 5) == 0 {
        write_all(stdout(), to_bytes("BUZ"));
    }
}

fn main() {
    fizbuz(1);
    fizbuz(2);
    fizbuz(3);
    fizbuz(4);
    fizbuz(5);
    fizbuz(6);
    fizbuz(7);
    fizbuz(8);
    fizbuz(9);
    fizbuz(10);
    fizbuz(11);
    fizbuz(12);
    fizbuz(13);
    fizbuz(14);
    fizbuz(15);
}
