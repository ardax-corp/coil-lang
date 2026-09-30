// CPU: StoreIndex + compound update hot loop.
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn main() {
    let arr = [0, 0, 0, 0, 0, 0, 0, 0];
    let i = 0;
    while i < 2000 {
        arr[i % 8] += 1;
        i = i + 1;
    }
    write_all(
        stdout(),
        to_bytes(
            format("%i", arr[0] + arr[1] + arr[2] + arr[3] + arr[4] + arr[5] + arr[6] + arr[7]),
        ),
    );
}
