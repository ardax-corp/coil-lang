// Prefetch on forward scans (locality step 6): 1M objects, old by the
// time a second vector lists them in a scattered order, then 10 walks of
// that vector. Each walk reads the object behind every element; the scan
// asks for the one 8 elements ahead while it reads this one.
use io::sync::println;
use string::format;

class P {
    pub x: int,
    pub y: int,
    pub z: int,
}

fn main() {
    let n = 1048576;
    let ps: Vec<P> = Vec::with_capacity(n);
    let i = 0;
    while i < n {
        ps.push(new P(i % 13, 1, 2));
        i = i + 1;
    }
    // An odd multiplier and an offset permute a power-of-two range.
    let order: Vec<P> = Vec::with_capacity(n);
    let k = 0;
    while k < n {
        order.push(ps[(k * 40503 + 12345) % n]);
        k = k + 1;
    }
    let t = 0;
    let r = 0;
    while r < 10 {
        let j = 0;
        while j < n {
            t = t + order[j].x;
            j = j + 1;
        }
        r = r + 1;
    }
    let _ = println(format("%i", t));
}
