// C2b / Q6 rung 4: coro for-in as ResumeCoro / DoneCoro (MIR dense or LIR).
// Helper sums yields. `main` stays format. Completion value is skipped.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

async fn gen() {
    let i = 0;
    while i < 64 {
        yield i;
        i = i + 1;
    }
}

fn coro_sum() -> int {
    let acc = 0;
    for x in gen() {
        acc = acc + x;
    }
    return acc;
}

fn main() {
    let total = 0;
    let round = 0;
    while round < 96 {
        total = total + coro_sum();
        round = round + 1;
    }
    write_all(stdout(), to_bytes(format("%i", total)));
}
