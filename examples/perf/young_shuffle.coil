// GC copy order (locality step 5): 16384 objects shuffled while young,
// then enough allocation for minor collections to promote them, then 2000
// walks of the vector. A minor copies an array's young elements in index
// order, so the walks read the copies one after another.
use io::sync::println;
use string::format;

class P {
    pub x: int,
    pub y: int,
    pub z: int,
}

fn main() {
    let n = 16384;
    let ps: Vec<P> = Vec::with_capacity(n);
    let i = 0;
    while i < n {
        ps.push(new P(i % 13, 1, 2));
        i = i + 1;
    }
    let seed = 12345;
    let k = n - 1;
    while k > 0 {
        seed = (seed * 1103515245 + 12345) % 2147483648;
        let j = seed % (k + 1);
        let t = ps[k];
        ps[k] = ps[j];
        ps[j] = t;
        k = k - 1;
    }
    let junk = 0;
    let r = 0;
    while r < 40 {
        let tmp: Vec<P> = Vec::new();
        let q = 0;
        while q < 20000 {
            tmp.push(new P(q, r, 0));
            q = q + 1;
        }
        junk = junk + tmp[r].x;
        r = r + 1;
    }
    let t = 0;
    r = 0;
    while r < 2000 {
        let j = 0;
        while j < n {
            t = t + ps[j].x;
            j = j + 1;
        }
        r = r + 1;
    }
    let _ = println(format("%i %i", t, junk));
}
