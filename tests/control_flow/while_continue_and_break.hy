// Fused `if i == k { continue/break }` inverts to `*Jmpt` (COI-87). Sum is
// 0+1+2+4+5+6 = 18 (skip 3, stop before 7).
// Output: 18
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let sum = 0;
    let i = 0;
    while i < 10 {
        if i == 3 {
            i = i + 1;
            continue;
        }
        if i == 7 {
            break;
        }
        sum = sum + i;
        i = i + 1;
    }
    write(stdout(), to_bytes(format("%i", sum)));
}
