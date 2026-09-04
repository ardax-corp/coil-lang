// Two-slot Result `?` / bind: helpers RETURN Result<int, int>; the
// hot path chains `?` without boxing an ObjEnum. ITERS=20000000;
// period-10 checksum 68 * 2e6 = 136000000. Release VM-only ~1-3s.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn step(int i) -> Result<int, int> {
    if i % 5 == 0 {
        return Result::Err(-1);
    }
    return Result::Ok((i % 10) + 1);
}

fn chain(int i) -> Result<int, int> {
    let a = step(i)?;
    let b = step(i + 3)?;
    return Result::Ok(a + b);
}

fn main() {
    let iters = 20000000;
    let acc = 0;
    let i = 0;
    while i < iters {
        acc = acc + match chain(i) {
            Result::Ok(v) => v,
            Result::Err(e) => e,
        };
        i = i + 1;
    }
    write_all(stdout(), to_bytes(format("%i", acc)));
}
