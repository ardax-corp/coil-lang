// Canary: interned string keys, dict increment, intern/hash pressure.
// A later intern/hash or dict-field cut should drop VM-only time vs this.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

fn main() {
    let counts = {
        a: 0,
        b: 0,
        c: 0,
        d: 0,
        e: 0,
        f: 0,
        g: 0,
        h: 0,
    };
    let words = Vec::from([
        "alpha",
        "bravo",
        "charlie",
        "delta",
        "echo",
        "foxtrot",
        "golf",
        "hotel",
        "india",
        "juliet",
        "kilo",
        "lima",
        "mike",
        "november",
        "oscar",
        "papa",
    ]);
    let buckets: Vec<int> = Vec::with_capacity(64);
    let b = 0;
    while b < 64 {
        buckets.push(0);
        b = b + 1;
    }
    let acc = 0;
    let i = 0;
    while i < 200000 {
        let k = i & 7;
        if k == 0 {
            counts.a += 1;
        } else if k == 1 {
            counts.b += 1;
        } else if k == 2 {
            counts.c += 1;
        } else if k == 3 {
            counts.d += 1;
        } else if k == 4 {
            counts.e += 1;
        } else if k == 5 {
            counts.f += 1;
        } else if k == 6 {
            counts.g += 1;
        } else {
            counts.h += 1;
        }
        let w = words[i & 15];
        let h = w.hash();
        acc = acc + h;
        let slot = h & 63;
        buckets[slot] = buckets[slot] + 1;
        i = i + 1;
    }
    let sum = counts.a + counts.b + counts.c + counts.d + counts.e + counts.f + counts.g + counts.h;
    let j = 0;
    while j < 64 {
        acc = acc + buckets[j];
        j = j + 1;
    }
    write_all(stdout(), to_bytes(format("%i", sum + (acc & 65535))));
}
