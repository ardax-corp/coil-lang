// @output high\nlow\n
// A `#[repr(int)]` enum is an immediate word like `int`, so a spawned
// closure may capture one and a thread may take or return one (#1904).
use io::sync::println;
use thread::{join, spawn};

#[repr(int)]
enum Lvl {
    Low = 1,
    High = 2,
}

fn name(Lvl l) -> string {
    return match l {
        Lvl::Low => "low",
        Lvl::High => "high",
    };
}

fn threaded(Lvl x) -> Lvl {
    let th = match spawn(fn () use (x) => x) {
        Ok(t) => t,
        Err(_) => panic "spawn",
    };
    let back: Lvl = match join(th) {
        Ok(v) => v,
        Err(_) => panic "join",
    };
    return back;
}

fn flipped(Lvl x) -> Lvl {
    let th = match spawn(
        fn () use (x) => match x {
            Lvl::Low => Lvl::High,
            Lvl::High => Lvl::Low,
        },
    ) {
        Ok(t) => t,
        Err(_) => panic "spawn",
    };
    let back: Lvl = match join(th) {
        Ok(v) => v,
        Err(_) => panic "join",
    };
    return back;
}

fn main() {
    println(name(threaded(Lvl::High)))?;
    println(name(flipped(Lvl::High)))?;
}
