// @output 20 5\n
//
// An imported type's static whose short name is also a host native
// (`Pool::with_capacity`, `Pool::format`) is that type's code, not the
// native: its result here is an object the caller does not own, so early
// freeing must leave it alone.
use io::sync::println;
use string::format;
use shared_pool::{Pool, Keeper};

fn size_via_capacity(Keeper k) -> int {
    let p = Pool::with_capacity(k, 4);
    return p.items.len();
}

fn size_via_format(Keeper k) -> int {
    let p = Pool::format(k);
    return p.items.len();
}

fn main() {
    let k = new Keeper(Vec::new());
    let i = 0;
    let total = 0;
    while i < 5 {
        total += size_via_capacity(k) + size_via_format(k);
        k.kept[0].items.push(i);
        i += 1;
    }
    println(format("%i %i", total, k.kept[0].items.len()))?;
}
